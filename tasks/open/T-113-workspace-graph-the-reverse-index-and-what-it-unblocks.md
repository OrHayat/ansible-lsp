# T-113 — Workspace graph: the reverse index and what it unblocks

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | epic | P2       | L    | —          |

## Problem

Everything this project answers today runs **forwards**: this reference points at that file.
Six open tickets need the arrow reversed — who points at *this* file — and each has been
sitting behind T-020 rather than building a partial version of it.

T-020's own argument is the epic's: *"Building it once, properly, is cheaper than four
partial versions."* The dependency graph already agrees — T-020 blocks more open tickets than
anything else on the board:

| Child | Needs the graph for |
| ----- | ------------------- |
| T-012 (P1) | invalidating exactly the files whose diagnostics a change can affect |
| T-068 (P1) | deriving `role_path` from real invocation chains instead of folder shape |
| T-021 | reachability — a file nothing points at |
| T-022 | cycles |
| T-054 | variable references, workspace-wide |
| T-079 | rewriting every reference to a plugin being moved |
| T-024 | the execution tree, rooted at a chosen playbook |

Two P1s sit behind a P2, which is the strongest argument for doing T-020 next and the reason
this epic is P2 while containing P1 work.

**T-011 is adopted deliberately, rejected.** It tried to expose this data through
`textDocument/callHierarchy` and could not: `prepareCallHierarchy` ignored the cursor and
answered every position with the whole file's calls, because call hierarchy is symbol-scoped
and this data is file-scoped. That is why T-020 exposes a custom `ansible/whoReferences`
rather than a standard request, and why T-024 is a TreeView rather than a protocol feature.
Keeping the rejection in the epic keeps the reasoning where the next person will look — it is
the one child here that is finished by being abandoned.

## Children

- [x] T-011 — Execution tree via LSP call hierarchy (**rejected** — see above)
- [ ] T-012 — File watcher and precise invalidation
- [ ] T-020 — Reverse index
- [ ] T-021 — `unused-file` / `unused-role` as faded hints
- [ ] T-022 — `circular-include` warning
- [ ] T-024 — Execution tree as a TreeView
- [ ] T-054 — Find variable references (the reverse of go-to-definition)
- [ ] T-068 — `role_path` from invocation chains, not folder shape
- [ ] T-079 — "Extract to collection": move a local plugin/module and rewrite every reference

## Done when

- [ ] every child is closed or rejected
- [ ] the graph is built once and shared, not rebuilt per consumer
- [ ] T-012 and T-068 — the two P1s — are among the first children closed, since they are why
      this is worth doing before the P3s that also want it
