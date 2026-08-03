# T-068 — `role_path` from invocation chains, not folder shape

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P1       | L    | T-020      |

## Problem

`expand_magic` (`resolve.rs:126`) claims `role_path` has "exactly one possible value: the
role this file belongs to", derived from folder shape (`find_role`, `workspace.rs:144`).
Ansible disagrees: `role_path` is injected per task only `if task._role`
(`vars/manager.py:478-481`), and `_role` is stamped at load time by whichever role *loaded*
the task (`playbook/task.py:99`). The value follows the invoker, not the file:

- a file no role-invocation chain reaches → `role_path` is **undefined**, the run crashes
- a file included cross-role → `role_path` is the *invoker's* dir, not the containing one

So both resolutions and missing-file **warnings** built on the shape guess can be wrong,
and a wrong warning is the tool lying.

## Design

Role context is computed per **chain**, walking the reverse index (T-020) from
`roles:` / `include_role` / `import_role` / meta-dependency sites down include edges.
Context propagates: every file below the role's entry point inherits its role dir. At each
role hop, name → dir uses the existing resolver (post-T-067 order) — no new lookup logic.

- `{{ role_path }}` expands to the **set** of chain-derived role dirs (deduped; usually
  size 1). Missing-file check: try each, a hit on any counts — the `playbook_dir` policy.
- Memoize context per file; cycle-guard the walk (T-022 machinery).

Two diagnostics:

1. **missing-file, chain-verified** — the reference resolves under no chain's role dir;
   message lists the chain(s) and paths tried. The warning stays, and can no longer lie.
2. **role_path-outside-role** — the file uses `role_path` and a *known* chain reaches it
   with no role context (e.g. `include_tasks` straight from a play): that chain crashes at
   runtime. Fires on positive evidence only — templated includes make edges unknowable, so
   "no role chain found" proves nothing, and absence of chains stays silent.

## Done when

- [ ] `role_path` expansion uses chain sets; the shape heuristic no longer feeds warnings
- [ ] cross-role include gets the invoker's dir, pinned by fixture
- [ ] rule 2 fires on a play-level `include_tasks` into a `role_path`-using file, and stays
      silent when the only chains are role chains or none are known
- [ ] a live two-chain run (role chain + task chain into the same file) confirms both the
      value and the crash, recorded in Settled

Source: `~/ansible_source/lib/ansible/vars/manager.py:478-484`,
`~/ansible_source/lib/ansible/playbook/task.py:99,495-497`
