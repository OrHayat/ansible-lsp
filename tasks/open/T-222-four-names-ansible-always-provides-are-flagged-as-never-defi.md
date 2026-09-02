# T-222 — Four names ansible always provides are flagged as never defined: role_names, inventory_file, role_uuid, environment

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | S    | T-112 | —          |

## Symptom

A task that reads `{{ role_names }}`, `{{ inventory_file }}`, `{{ role_uuid }}` or
`{{ environment }}` gets the undefined-variable diagnostic. Ansible provides all four, so the
squiggle is a lie of the kind the first paragraph of `CLAUDE.md` is about.

Measured, both halves. `undefined_uses` on a one-task playbook whose `debug` message reads
six names:

    {{ role_names }} {{ inventory_file }} {{ role_uuid }} {{ environment }} {{ groups }} {{ inventory_dir }}

flags exactly `["role_names", "inventory_file", "role_uuid", "environment"]`. The last two
are the control — same expression, same rule, silent. And on ansible-core **2.21.2**, a
playbook that lists its own variable names with `query('varnames', '.*')` at three points
(play-level task, a looped task, a task inside a role), non-`ansible_`-prefixed names only:

| level | present                                                                                                                                                   |
| ----- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| play  | `environment` `group_names` `groups` `hostvars` `inventory_dir` `inventory_file` `inventory_hostname` `inventory_hostname_short` `play_hosts` `playbook_dir` `role_names` `vars` — and `omit`, which `varnames` does not list but `omit is defined` answers `True` |
| loop  | the play set plus `item`                                                                                                                                  |
| role  | the play set plus `role_name` `role_path` `role_uuid`                                                                                                     |

Also present but not a user-facing name: `_task`.

## Cause

`condition::MAGIC` is a hand-typed list and was never diffed against a run. It has
`inventory_dir` but not `inventory_file`; `role_name` and `role_path` but not `role_names`
(play-level, from `_get_magic_variables`) or `role_uuid`; and nothing for `environment`,
which `VariableManager.get_vars` sets unconditionally. `is_injected` — the one predicate the
undefined rule, hover and the injected-hover path all read — is `MAGIC` plus the `ansible_`
prefix, so every consumer inherits the gap at once, which is rule 3 working as intended:
one list to fix.

Reading the source alone would not have found this. `_get_magic_variables` is where
`role_names` and `role_uuid` live, but `inventory_file`/`inventory_dir` arrive through
`self._options_vars` (a loop over a dict, no literal key to grep), `omit` is a templar
special that no `variables[...] =` sets, and `environment` is in `get_vars` not the magic
function. The `varnames` run is the measurement; the source is only where to look next.

## Fix

Add the four to `MAGIC`, with a comment on each saying which layer sets it and that the
list was diffed against a 2.21.2 `varnames` run — so the next person knows it was measured,
not typed. Keep `play_hosts`: it is present, though 2.21 tags it deprecated (a hover fact
for later, not this ticket).

Not in scope, but recorded: `item` and `role_name`/`role_path`/`role_uuid` are only present
in a loop / in a role, and `MAGIC` treats all of them as always-present. That is the
opposite error — silence where a diagnostic is due — and a separate, larger ticket if it is
ever worth chasing; [[T-217]]'s builtin-modifier work has the same scope question for
Jinja's `loop`.

## Done when

- [ ] the six-name playbook above flags nothing — a test in `vars.rs` asserting the exact
      empty set, next to the existing `undef` helper
- [ ] the same test carries a control that still flags: a seventh, genuinely undefined name
      in the same expression, so the fix cannot pass by silencing the rule
- [ ] every consumer of `is_injected` is listed in the test's doc comment with what it
      answers for `inventory_file` (rule 3: a test per consumer, not per rule) — hover,
      injected-hover, and the undefined rule
- [ ] `MAGIC`'s comment names the ansible-core version it was diffed against and the
      `varnames` recipe, so the next drift is one command to find
