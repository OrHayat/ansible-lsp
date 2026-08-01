# T-044 — Semantic AST (Play / Block / Task / Role)

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| done   | P1       | L    | T-036      |

## Problem

Everything walked the raw `Node` (YAML CST) and pattern-matched keys — fine for navigation,
but the variable and execution features need real structure: task ordering, block nesting, and
knowing a task's *module* from its *directives*. Reinventing that per-feature is how the
call-hierarchy attempt (T-011) went wrong.

## Approach

A parallel `crate::ast` layer lifting `Node` into typed, span-carrying nodes:

```
Ast = Playbook(Vec<PlayItem>) | Tasks(Vec<Stmt>) | Other
PlayItem = Play | Import
Play  → roles, pre/tasks/post/handlers (ordered), vars, vars_files
Stmt  = Task | Block
Block → block/rescue/always (nested)
Task  → action (module) + when/loop/register/vars + directives
```

Built alongside the raw tree; no existing resolver reads it (that's T-047).

## Done when

- [x] a playbook and a task file parse into typed, ordered, nested nodes with spans
- [x] the module/directive split is schema-driven (T-045)
- [x] no behaviour change — parallel layer only

Resolution: commit `96c724b`.
