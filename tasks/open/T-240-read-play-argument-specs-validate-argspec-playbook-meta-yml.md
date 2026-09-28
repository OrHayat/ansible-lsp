# T-240 — Read play argument specs (validate_argspec + <playbook>.meta.yml) as the play's declared inputs

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | L    | T-123 | —          |

## Problem

ansible-core 2.20 added play argument specs (tech preview). A play opts in with the
`validate_argspec` keyword, and its inputs are declared in a meta file next to the playbook:

```yaml
# playbooks/expose.yml
- name: expose share
  hosts: storage
  validate_argspec: true
  tasks:
    - debug: msg="{{ share_name }} {{ port }}"
```

```yaml
# playbooks/expose.meta.yml
argument_specs:
  expose share:
    options:
      share_name: {type: str, required: true}
      port:       {type: int, required: true}
      squash:     {type: str, default: root_squash}
```

This is the playbook-level twin of T-041's role `meta/argument_specs.yml`: a declared
signature for the play. We read none of it today, so a variable the play gets from its caller
gets no definition site. `vars.rs` says why: a name absent from the index "is *not* proof it's
undefined".

Measured on ansible-core 2.21.3 (probes in the session that filed this; each one had a control
that came out different):

- `validate_argspec: true` → the spec name is the play's `name:`. A play with no name fails at
  load: "A play name is required when validate_argspec is True".
- `validate_argspec: <string>` → the spec with that name.
- Meta file lookup, in order from the error text: `<stem>.meta.yml`, `.meta.yaml`, `.meta.json`,
  `.meta`. A missing file ("A playbook meta file is required") and a spec name absent from it
  ("No argument spec named '…'") are both load-time errors, before any task runs.
- Ansible inserts a task, "Validating arguments against arg spec <name>". A missing required
  option fails that task. The control without the keyword fails later, at the first read, as
  "'share_name' is undefined".
- Types are enforced: `type: int` rejects `abc` ("unable to convert to int"). `choices` is
  enforced. `type: str` accepts anything by converting it.
- Validation checks the value **but does not convert the variable**. The declared type says
  nothing reliable about what the play receives, in either direction:

  | spec        | passed                  | check | `type_debug` in the play |
  | ----------- | ----------------------- | ----- | ------------------------ |
  | `type: int` | `-e port=80`            | pass  | `str`                    |
  | `type: int` | `-e port=abc`           | fail  | —                        |
  | `type: str` | `-e version=1`          | pass  | `str`                    |
  | `type: str` | `-e '{"version": 1}'`   | pass  | `int`                    |
  | `type: str` | `-e '{"version": 1.5}'` | pass  | `float`                  |
  | `type: str` | `-e '{"version": [1]}'` | pass  | `list`                   |

  `key=value` extra vars are always strings; JSON extra vars keep their JSON type. A
  `type: str` option can therefore hold an int, a float or a list, and a `type: int` option can
  hold a string. So no reader may treat the declared type as the value's type: not hover, not
  a future type-aware diagnostic, not T-182's value domain.
- It checks every var in scope, not just the caller's: a required option set in play `vars:`
  passes.
- **Spec defaults are not injected.** Reading `squash` unsupplied is undefined at runtime.
- **The spec is not closed.** An undeclared name passed by the caller is accepted and readable.
  So a name outside the spec is *not* proof it is undefined.

## Why it works this way upstream

Each point below is what the source and the upstream threads say. The first three are measured
above; don't re-derive them, and don't "fix" our model toward what users keep asking for.

**How the check runs.** `play_iterator.py` inserts one `validate_argument_spec` task after
play-level fact gathering. Its action (`plugins/action/validate_argument_spec.py`,
`get_args_from_task_vars`) walks the **spec's** option names and looks each one up in the
task vars — every source, not only the caller's. It never enumerates the task vars, so a name
the spec does not declare is never looked at. That is the whole reason the set is open.

**It is a runtime task, and it is not free.** Measured with two hosts and `gather_facts: true`:
"Gathering Facts" runs on both, *then* "Validating arguments against arg spec" runs **once per
host** and fails on each. So a missing input is caught before any change is made, but only after
connecting to every host and gathering facts. The spec is re-checked per host, for a question
(what did the caller pass?) whose answer is the same for all of them. The closed form below makes
that task heavier still: a Jinja loop over every host var, per host.

This is the case for doing it statically. The same file, read at edit time, costs nothing at run
time. And because the editor can hold the author to the declared inputs, it can give the
closed-set check that Ansible itself only offers through a hack.

**Extra variables are allowed on purpose.** The feature PR, ansible/ansible#85763, says:
"Undocumented variables are ignored by default. module_defaults can define the optional
argument `provided_arguments` to ensure there's documentation for arbitrary options." A play
reads inventory vars, facts, group_vars; a spec that rejected every undeclared name would make
every such play fail. `provided_arguments` is the escape hatch. It is a parameter of that
inserted task, a dict of extra values to validate, and a key the spec does not declare fails
with "Unsupported parameters". Upstream's integration test
(`test/integration/targets/play_arg_spec/playbooks/module_defaults.yml`) closes the spec by
feeding it every host var through a Jinja loop:

```yaml
module_defaults:
  validate_argument_spec:
    provided_arguments: |
      {% set d = {} %}
      {% for k, v in hostvars[inventory_hostname].items() %}
      {% if v is defined and not k is search("^ansible_") %}{% set _ = d.update({k: v}) %}{% endif %}
      {% endfor %}{{ d }}
```

Measured on 2.21.3: with that `module_defaults`, `-e extra=E` fails "extra. Supported parameters
include: share_name."; without the extra it runs. It is opt-in, and it is the only way to get a
closed spec. Nothing upstream plans a cleaner one.

**Opt-in by keyword, not by the file's existence.** In #85763's review, a reviewer asked whether
the meta file alone should turn validation on, as it does for roles. Declined. A play is
renamed more easily than a role entry point, and with implicit validation a rename (or a
templated play name that renders differently) would silently switch it off. With the keyword,
a mismatch is a load-time error. The `true | <spec name>` single keyword was the
architecture-doc decision, kept after discussion.

**Defaults are documentation only.** This has been asked for repeatedly on role specs and closed
as working as designed every time: #85712, #80604 (duplicate of #80298), and #77664 ("Read
defaults from `meta/argument_specs.yml`", closed after core had decided against it at design
time). The reason, from #80604: the validator cannot inject new or converted variables into the
run without breaking variable precedence, so real defaults belong in `defaults/main.yml`, or,
for a play, its `vars:`. The play feature uses the same validator, which is why our probe read
`squash` as undefined.

**Types are checked by conversion, not strict match.** Upstream calls it a "coercing
validator": a value passes if it *can* convert to the declared type. `abc` cannot become an
int, so it fails. `"80"` can, so it passes, and the variable **stays a string**, because
nothing is written back (same reason as defaults). `str` accepts almost anything. Closed as
expected behaviour in #80571, #78889 and #87217; #77159 asked for stricter checks. Strict mode is
in progress as ansible/ansible#87226 (`type_args`, open since 2026-07-07). Until it merges,
hover must say "declared int", never "is an int".

**Validation can be skipped.** The inserted task inherits the play's tags. The 2.20.5 / 2.21.0
changelog: "The `always` tag is only added if the play has no tags". So with play tags,
`--tags`/`--skip-tags` can skip validation entirely, and upstream's own test covers
`--skip-tags play_level_tag`. #86345 fixed an interaction with `--start-at-task`. The mirror
image for roles is still open: #82505, validation running for roles that tags should have
excluded. So "required means defined" is the author's declared contract, not a runtime
guarantee.

**Templating.** `_post_validate_validate_argspec` templates a string `validate_argspec`, and a
`true` falls back to `self.name`, which can itself be templated. When either doesn't resolve
statically, we cannot know which spec applies.

**Tooling state.** The docs page covers none of the behaviour above. ansible-lint flagged
`validate_argspec` as an invalid play keyword until ansible-lint#5187 (merged 2026-09-20,
fixing #5168).

Related: T-061 infers a playbook's required inputs from unguarded uses. For a play with a spec,
the spec is the declared answer, and the two should agree.

## Approach

Treat the spec as the play author's declared contract. A play that opts in has said what its
inputs are, so reading anything else is a contract violation. That is a true statement whatever
the caller does at runtime.

- **Definitions.** Only `required: true` options count as defined for the play, since only they
  are guaranteed past the validation task. Optional options are *declared*, not defined.
- **Go-to-definition / hover.** Point into the meta file's option line. Hover shows the type,
  choices and default. Say "declared int", never "is an int": the value is not converted.
- **Warning.** In an opted-in play, flag a name that is neither declared in the spec nor
  defined anywhere in the workspace, nor injected. Word it as the contract, never as
  "undefined": "`x` is not declared in the argument spec for play `P` (<file>) and is not
  defined in the workspace".
- **Hint.** Flag an option that is declared but never read.
- **Load errors.** Missing meta file, missing spec name, `true` on a nameless play.
- **Templated names.** If `validate_argspec` or, for `true`, the play name is templated and does
  not resolve statically, say nothing. No spec, no warnings, no load error. Guessing the spec
  is exactly the confident wrong answer this repo exists to avoid.
- **Closed specs.** A play whose `module_defaults` sets `validate_argument_spec.provided_arguments`
  gets real rejection of undeclared names from Ansible itself. Recognising that pattern is
  worth it: the undeclared-name warning then matches a runtime failure, not just the contract.
- Plays without `validate_argspec` keep today's behaviour exactly.

Open, and to be measured rather than assumed:

- Reach. Does "read in the play" cover included task files and roles reached from it? Follow
  existing scope rules, per rule 3.
- An optional option read without `| default`. Undefined at runtime when unsupplied, so it may
  deserve its own hint.
- The 2.20.5 / 2.21.0 changelog note on tags ("the `always` tag is only added if the play has no
  tags") is unmeasured here.

Docs: https://docs.ansible.com/projects/ansible-core/devel/playbook_guide/playbooks_variables_validation.html

## Done when

- [ ] the meta file is found by the four spellings in Ansible's order, and the spec is keyed off
      `true` → play name, or the string
- [ ] required options count as defined in the play; optional ones do not; every reader of the
      index answers the same (a test per consumer)
- [ ] go-to-definition lands on the option line; hover shows type/choices/default as declared
- [ ] an undeclared, workspace-undefined name in an opted-in play warns with the contract
      wording; the same read in a play without the keyword is unchanged
- [ ] a declared option never read gets a hint
- [ ] missing meta file, missing spec name, and nameless `true` each get a diagnostic
- [ ] a templated `validate_argspec` or play name that does not resolve produces nothing
- [ ] hover never states a converted type or an injected default; the declared type feeds no
      inference about the value (a test with `type: str` holding an int pins it)
- [ ] a demo fixture pins each label, with an `every_other_demo_file_is_free_of_<rule>` guard
