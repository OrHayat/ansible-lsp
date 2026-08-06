# T-122 — changed_when, failed_when and until are the same expression language

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-121 | —          |

## Problem

`condition.rs` handles `when:`. It is scoped to about a quarter of its actual surface, because
three more keywords take the identical expression language and get none of the analysis.

`Task.post_validate` deliberately does **not** template them — it returns the value unchanged
and leaves evaluation for later (`playbook/task.py:388-393, 445-464`):

| Keyword | Evaluated as | Where |
| ------- | ------------ | ----- |
| `changed_when` | conditional list | `executor/task_executor.py`, after the module runs |
| `failed_when` | conditional list | same |
| `until` | conditional, per retry | same |
| `when` | conditional | `_engine.py:490+` |

Same parser, same strictness rules, same undefined-variable behaviour. So every rule the
`when-*` family already ships applies unchanged: variables defined nowhere (T-033), the four
provable-fault shapes (T-032), and the 2.19 strictness cases (T-117).

Two differences worth encoding rather than assuming away:

- `until` runs **after** the task, so `register`'s variable is in scope for it — and is *not*
  in scope for that task's own `when:`. A definedness rule that ignores this will produce
  false positives on `until: result.rc == 0`, which is the single most common `until` there is.
- `changed_when: false` and `failed_when: false` are idiomatic and must never be flagged as
  constant-condition faults, even though a constant `when: false` is worth a hint.

## Approach

Widen the reference extraction to the four keywords and let the existing rules run over all
of them. The work is scope plus the two exceptions above, not new analysis.

## Done when

- [ ] all four keywords produce condition references
- [ ] the existing `when-*` rules apply to them
- [ ] a `register`ed variable is in scope for that task's `until:` and not its `when:`
- [ ] `changed_when: false` / `failed_when: false` never warn
- [ ] the corpus reports a count per keyword, so the added surface is measured
