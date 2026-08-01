# T-047 — Move the resolvers onto the AST

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| done   | P1       | L    | T-044      |

## Problem

The AST (T-044) was dead code until something read it. Proving it's sufficient means moving
the existing walkers off the raw tree — the risky migration that must not change behaviour.

## Approach

- `references::extract` now walks the `Ast` instead of the raw `Node` tree; `Action` retains
  its arg node and `Task`/`Block`/`Import` carry parsed `when:` + loop context.
- `mutation::collect_assignments` walks the AST for `set_fact`/`register`; `Task` gained a
  `register` field.
- `condition.rs` needed no change — it matches `when:` strings already supplied by the
  AST-based references.

Gated by a `scan` snapshot over the demo and fixtures.

## Done when

- [x] references + mutation read the AST, not the raw tree
- [x] `scan` output byte-identical on demo and fixtures (behaviour preserved)
- [x] all tests pass

Resolution: commits `1e0aff5` (references), `ba03158` (mutation).
