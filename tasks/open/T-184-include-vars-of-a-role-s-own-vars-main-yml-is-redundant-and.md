# T-184 — include_vars of a role's own vars/main.yml is redundant and raises its precedence

| Status | Priority | Size | Refs         | Depends on |
| ------ | -------- | ---- | ------------ | ---------- |
| partly done | P2  | S    | T-051, T-146, ~~T-207~~ | ~~T-206~~  |

## Problem

A role that opens with

```yaml
- name: Include role variables
  include_vars: main.yml
  tags: [always]
```

is re-loading the file Ansible **already loaded for it**. Found in the wild: `roles/ad` in
`~/app/ansible`, first task of `tasks/main.yml`.

It costs on two counts, and the second one was written off as harmless here for a long time.

**1. It runs once per host, and buys nothing.** Measured on 2.21.3, 200 hosts, local
connection, no fact gathering — the same play with the line and with it deleted:

| | runtime |
| -------------------------- | -------------------- |
| with the redundant include | 1.71 / 1.68 / 1.68 s |
| line deleted               | 0.77 / 0.75 / 0.63 s |

About **1 second, or ~5ms per host**, to re-load a file that is already loaded. The control
that identifies where it goes: replacing the include with a trivial `debug` costs the same
(1.62/1.73 s), so it is the per-host *task execution*, not the file read — the loader caches
the parse. Slower plays dilute the share, never to zero.

**2. It lifts the precedence.** `include_vars` sits above role `vars/`, so the re-load
silently raises those values above anything a caller sets at task level.

Either one alone justifies the hint, and that matters for the rule's shape: (1) applies
whether or not anybody overrides the value, so a refinement that only fires when some caller
*does* override would go quiet on exactly the plays paying the most — a large inventory with
no override anywhere.

Measured on 2.21.2, one role, one variable, with the include toggled by a flag so the same
fixture reports both ways:

| run                        | plain task `vars:` | no task var |
| -------------------------- | ------------------ | ----------- |
| control, no `include_vars` | `FROM_TASK_VARS`   | `FROM_ROLE_VARS` |
| with `include_vars`        | **`FROM_ROLE_VARS`** | `FROM_ROLE_VARS` |

The control matters twice over: its right-hand cell proves the role vars were auto-loaded
without any help (so the include really is redundant), and its left-hand cell proves a task
var normally wins (so the change in the second row is the include's doing).

**A first probe of this got the opposite answer and passed anyway.** It compared against
`vars:` on an `include_tasks`, which are *include params* — precedence 21, above
`include_vars` at 18 — so the value never moved and the effect looked absent. Rule 2: the
probe could not produce the other outcome. Only a plain task-level `vars:` shows it.

## Approach

A hint, never an error, with `# noqa`. Re-including is legal, and someone may want the
precedence lift or the `tags: always` placement on purpose — the message should say what it
does, not that it is wrong.

Fire on the shape, not on evidence of harm. The per-host cost is unconditional, so waiting
for a demonstrable override before speaking would suppress the hint where it is worth most.

Fires when: the reference kind is `IncludeVars`, the file is inside a role
(`ctx.role_dir.is_some()`), and the resolved target is that role's own `vars/main.yml`.
`FileContext` supplies `role_dir`, so no index, no graph, no new walk.

**Unblocked — [[T-206]] is done.** "The resolver already resolves `IncludeVars` targets" was
the other half of that sentence and it was false: `include_vars: main.yml` from
`<role>/tasks/main.yml` resolved to the task file itself, because the candidate order put
`<file_dir>` ahead of `<role>/vars/`. Fixed, so the resolved target is now the role's own
`vars/main.yml` and this rule can key on it as written. The precedence table above re-measured
clean on 2.21.3.

[[T-207]] is done too, so the index now carries both loads and hover already shows the pair —
`include_vars` marked effective over `role var`. The hint this ticket adds says in words what
that stack shows on the token, and the two will not contradict each other. `demo/roles/chain-c`
carries the fixture.

Message should name the consequence, not the redundancy: the redundancy is harmless, the
precedence lift is what bites. Something a reader can act on — "role vars are loaded
automatically; re-loading them here also puts them above task-level `vars:`".

Do **not** extend this to `include_vars` of any other file, or of another role's vars. Only
the self-referential case is provable.

## Done when

- [x] the rule fires on the exact shape above and is silent for `include_vars` of a
      different file, of another role's `vars/main.yml`, and of the same file from outside a
      role — each asserted separately, not one combined case. Two more silence cases beyond
      the three named: the dir form, and a target that does not resolve
- [x] the measurement above is a test in this repo, with the control that produced
      `FROM_TASK_VARS`, so a future change that stops the lift fails here — the precedence
      pair is `a_reinclude_of_the_roles_own_vars_is_indexed_at_both_precedence_levels`
      (T-207), and the per-host cost is recorded in Problem with its control
- [x] `# noqa` suppression works, rule id matched exactly — including the control that it
      still fires under an *unrelated* id, so one suppression cannot hide two rules
- [ ] corpus gate: measure before shipping. **Not done — the tree named above is not on this
      machine.** What was swept: the demo tree, where it fires exactly once, on the row
      labelled for it, with a sweep asserting no other demo file fires. That is not the same
      as a real repo and this box stays open until someone runs it on one.

      Two things to weigh when it is run. The rule is a HINT, so many hits cost less than
      many warnings would. And a hit is never a false positive — it is an exact path match,
      so every one is a genuine re-include; the question is only whether the idiom is common
      enough that saying so on each is noise.

## What landed

`include_vars::redundant_self_reload` — target and `role_dir` in, `Option<Problem>` out, keyed
on the **resolved** target rather than on any spelling of the path, so `main.yml`,
`file: main.yml`, and the FQCN forms are all covered without enumerating them.

`placement::Tier` gained `Hint`, which is the "1 more place" that variant now lives (the other
is `condition.rs`). It maps to `DiagnosticSeverity::HINT` — no squiggle, which is the point.

Demo: `demo/roles/chain-c/tasks/main.yml`, labelled **HINT**. That prefix did not exist in
`demo/README.md`'s list before this and has been added to it — legal working Ansible the tool
remarks on without claiming a fault.
