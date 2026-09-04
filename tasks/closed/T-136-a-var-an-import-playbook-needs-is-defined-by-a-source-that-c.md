# T-136 — A var an import_playbook needs is defined by a source that cannot reach it

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P1       | M    | T-099 | T-095      |

## Problem

```yaml
# group_vars/all.yml
env: prod

# site.yml
- import_playbook: "{{ env }}-setup.yml"
```

This never works. `group_vars` cannot supply a value to an `import_playbook`, so the author's
definition is **inert** — it looks like it configures the import and does nothing. The whole
playbook dies at parse time with `'env' is undefined`, and the editor, which has both halves
of this in its index, says nothing about the connection.

Split out of T-095. T-095 stops the wrong message; this ticket adds the one thing neither
Ansible nor ansible-lint can say — *which definition the author is relying on, and why it is
inert*.

## Why it happens

`import_playbook` is templated when the playbook file is **parsed**, by
`variable_manager.get_vars()` called with no play, host or task
(`playbook_include.py:69-83`). Every source that needs one of those three is out of scope.
T-095 carries the full table and the live run against ansible-core 2.21.2 confirming it;
short version: only `-e` and a literal `vars:` on the import entry work.

Now line that up against `VarSource` (`vars.rs:31-57`), everything the index knows:

| `VarSource` | Needs | Can reach a parse-time import? |
| ----------- | ----- | ------------------------------ |
| `PlayVars`, `BlockVars`, `TaskVars` | a play / block / task | no |
| `SetFact`, `Register` | a host, at runtime | no |
| `VarsFiles` | a play | no |
| `RoleDefaults`, `RoleVars` | a play | no |
| `GroupVarsAll`, `GroupVars`, `HostVars` | a host | no |
| `IncludeVars` | a task | no |

**All twelve are unreachable.** Extra-vars is deliberately not indexed (`vars.rs:60-61`
— precedence 22, outside what a static index can see), and a `vars:` on the import entry
is not indexed at all today. So the rule needs no per-source classification: *if the index
resolves the name, that definition cannot be the one supplying it.*

## What this must NOT claim

**Not "the playbook is broken".** A variable in `group_vars` can also be passed with
`-e env=prod` at launch — group_vars as the default, `-e` as the override, is an ordinary
pattern. The command line is invisible to us, so **no case here is provably fatal**, and an
earlier draft of T-095 that said otherwise was wrong.

What *is* provable is T-095's half: the file cannot pass `ansible-playbook --syntax-check`
(exit 4, live-verified), because that command takes no user arguments. This ticket's addition
sits on top of that and is also certain, being a statement about the definition rather than
about the run:

> `env` is defined in `group_vars/all.yml:1`. That cannot supply an `import_playbook`, which
> is expanded before any host exists — only `-e env=…` or a `vars:` on this line can.

That is true regardless of the command line. It names the file the author actually wrote,
which is the part they will otherwise stare at for an hour.

Severity: **warning**, not error. It fires on a real misconception, but it cannot know the
launch command. If the corpus shows it is noisy, downgrade to a hint rather than widening it.

## Approach

1. On a templated `ImportPlaybook` reference, pull the variable names out of the template
   (`references.rs` already extracts uses for T-049).
2. Ask the var index for each name. A hit → emit, naming the defining file, line and
   `VarSource`. A miss → say nothing; that is T-095's "requires `-e`" case.
3. The `vars:` on the import entry has to be read first, or it is a false positive on the
   one spelling that *does* work — T-095 covers reading it, hence the dependency.

## Watch out

- Deriving "unreachable" from the `VarSource` list means a **new** variant added later is
  silently assumed unreachable. Make that a match with no wildcard arm, so adding a source
  fails the build until someone decides which side it is on.
- `demo/` needs the fixture: `group_vars/all.yml` defining `env`, a templated import using
  it, and the `-e` spelling next to it as the GOOD case.
- The rule needs the corpus run before it ships — `~/app/ansible` has 48 conditional imports;
  if a meaningful number are templated and this fires on all of them, the framing is wrong.

## Progress

Landed as `inert-import-var` (WARNING), `Backend::inert_import_var_diagnostics` in
`main.rs`, run from the publish path's `State::inventory_diagnostics` beside the coverage and
`unknown-host` rules — it reads the definition index, and that index depends on the inventory,
so it takes the same per-file inventory snapshot those two do (T-202). The reachability
question is `VarSource::can_supply_import_playbook` in `vars.rs`: an arm per variant, every
one `false`, no wildcard, so a variant added later does not compile until it is placed.

The message names the file, 1-based line and source of the winning definition
(`vars::effective` over the in-scope ones, with "and N more" when several), and stops at the
definition being inert. Names are taken only from inside `{{ }}` — the first draft scanned the
whole value as an expression and read `env.yml` as a use of `env`, which the magic-variable
control caught.

Silent, each pinned: a name nothing indexed defines (T-095's case), an entry with any `vars:`
at all, a magic-only template, an `import_playbook` inside a task list, the rule's own noqa
and a bare one. The control on the silence: `# noqa: templated-import` on the same line does
**not** silence this rule, since the two say different things. Seen red with the rule
short-circuited — the four positive tests fail, the demo guard stays green as a guard should.

Demo: `group_vars/all.yml` defines `deploy_stage`, and `playbook.yml` gained the row after
the SILENCED `{{ env }}` one. Not `env`, as the Watch-out suggested: `env` is deliberately
defined nowhere reachable from `playbook.yml` (that is what keeps T-095's rows on T-095's
side), and a second name keeps both fixtures honest. The `-e` GOOD case is the existing
SILENCED row. Pinned by `demo_playbook_has_exactly_the_documented_inert_import_var` plus the
`every_other_demo_file_is_free_of_inert_import_var_diagnostics` guard.

**The corpus gate**, run 2026-09-04 through `inert_import_var_corpus` (an env-gated
`#[ignore]`d test in `main.rs`, the same shape as T-184's, because the rule lives in the
publish path and `scan.rs` cannot reach it):

| tree | commit | yaml files | files mentioning `import_playbook` | templated playbook-level imports | hits |
| ---- | ------ | ---------- | ---------------------------------- | -------------------------------- | ---- |
| the reference tree (`~/app/ansible`, here at `volumez/matrix/ansible`) | `186c7ed5` | 768 | 57 | **0** | **0** |

The control came out different: `demo/` reports exactly 1, on the `deploy_stage` row. And
the zero denominator was checked outside the tool — a grep for `import_playbook:` lines
containing `{{` over the same tree also finds none; the 57 files match the grep's 57. So
the zero is honest but weak: this tree has no templated import for the rule to judge, and
the "48 conditional imports" the Watch-out worried about are `when:`-gated literal paths,
which the rule never looks at. The ticket's noise question stays open in principle and is
answered for this tree only; the gate stays runnable for the next one.

## Done when

- [x] a templated `import_playbook` whose var the index defines names that file and source
- [x] the message says the definition is inert, never that the playbook fails
- [x] a `vars:` on the import entry suppresses it entirely
- [x] a var the index does not know stays silent here (T-095 owns that case)
- [x] adding a `VarSource` variant fails the build rather than defaulting to unreachable
- [x] the corpus scan is run and the hit count recorded here — 0 hits over 0 templated imports, see Progress
