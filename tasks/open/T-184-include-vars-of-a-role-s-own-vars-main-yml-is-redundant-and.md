# T-184 — include_vars of a role's own vars/main.yml is redundant and raises its precedence

| Status | Priority | Size | Refs         | Depends on |
| ------ | -------- | ---- | ------------ | ---------- |
| open   | P2       | S    | T-051, T-146 | T-206      |

## Problem

A role that opens with

```yaml
- name: Include role variables
  include_vars: main.yml
  tags: [always]
```

is re-loading the file Ansible **already loaded for it**. Found in the wild: `roles/ad` in
`~/app/ansible`, first task of `tasks/main.yml`.

It is not merely redundant. `include_vars` sits at a higher precedence than role `vars/`, so
the re-load silently lifts those values above anything a caller sets at task level.

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

Fires when: the reference kind is `IncludeVars`, the file is inside a role
(`ctx.role_dir.is_some()`), and the resolved target is that role's own `vars/main.yml`.
`FileContext` supplies `role_dir`, so no index, no graph, no new walk.

**Blocked on [[T-206]].** "The resolver already resolves `IncludeVars` targets" was the other
half of that sentence and it is false for this exact shape: `include_vars: main.yml` from
`<role>/tasks/main.yml` resolves to **the task file itself**, because the candidate order puts
`<file_dir>` ahead of `<role>/vars/`. Measured both sides — Ansible 2.21.3 loads
`<role>/vars/main.yml`, we return `<role>/tasks/main.yml`. Written on top of that, this rule is
silent on the case it exists for, and its test would only pass on a fixture with no
`tasks/main.yml`. The precedence table above re-measured clean on 2.21.3, so the behaviour half
of this ticket stands as written.

Message should name the consequence, not the redundancy: the redundancy is harmless, the
precedence lift is what bites. Something a reader can act on — "role vars are loaded
automatically; re-loading them here also puts them above task-level `vars:`".

Do **not** extend this to `include_vars` of any other file, or of another role's vars. Only
the self-referential case is provable.

## Done when

- [ ] the rule fires on the exact shape above and is silent for `include_vars` of a
      different file, of another role's `vars/main.yml`, and of the same file from outside a
      role — each asserted separately, not one combined case
- [ ] the measurement above is a test in this repo, with the control that produced
      `FROM_TASK_VARS`, so a future change that stops the lift fails here
- [ ] `# noqa` suppression works, rule id matched exactly (see T-146 if suppression is
      centralised by then)
- [ ] corpus gate: measure before shipping. If it fires more than a handful of times on
      `~/app/ansible`, each hit is read before the rule lands — a hint that fires on a common
      idiom is noise, and the bar for adding one is that the reader would change the code
