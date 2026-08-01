# T-048 — Variable definition index (in-file + cross-file)

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| done   | P1       | L    | T-044      |

## Problem

The keystone of the variable model: know where every variable is *defined*. Absorbs the
navigation goals of T-016 (`vars_files`) and part of T-017 (`include_vars`) by treating them
as definition sources rather than standalone features.

## Approach

`crate::vars`:

- **Phase 1 — in-file** (`index`): play/block/task `vars:`, `set_fact:`, `register:` over the
  AST, keyed by name with source and span.
- **Phase 2 — cross-file** (`definitions`): follows deterministic paths only — role
  `defaults/`+`vars/`, `vars_files:` targets, and the `set_fact`/`register`/`vars:` in included
  task files and roles (reusing the include/role traversal, depth-capped, cycle-guarded). Each
  def carries its file. **No** folder scanning for group_vars/host_vars (ambiguous), no
  inventory.

## Done when

- [x] in-file defs collected with source + span
- [x] cross-file defs from role defaults/vars, vars_files, and reachable set_fact/register
- [x] each def records the file it lives in, for cross-file go-to-def
- [x] self-contained test over a temp playbook + vars file + role

Resolution: commits `b34bacd` (Phase 1), `09f328c` (Phase 2). Remaining sources
(`include_vars`, role params) tracked in T-053.
