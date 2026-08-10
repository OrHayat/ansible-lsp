# T-068 — `role_path` from invocation chains, not folder shape

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P1       | L    | T-113 | T-020      |

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

## The three cases, for `roles/a/tasks/x.yml`

| Who invoked it | `role_path` | Folder guess |
| -------------- | ----------- | ------------ |
| role `a` itself — its own `tasks/main.yml` includes it | `roles/a` | **right** |
| role `b` includes it cross-role | **`roles/b`** — the invoker's dir | **wrong** |
| a play includes it directly, no role in the chain | **undefined** — the run crashes | **wrong, and hides a crash** |

**The point is not that case 1 is hard — it is that from inside the file all three look
identical.** `x.yml` cannot tell whether `a`, `b`, or a bare play is running it, so we cannot
tell when the guess applies. Shipping it means being right most of the time and confidently
wrong the rest, with no way to know which — the reason the expansion is disabled rather than
kept as a heuristic.

Case 3 is why this ticket is worth more than a correction. Once the chains are known it stops
being "we cannot answer" and becomes a **new diagnostic**: this file uses `role_path`, a known
chain reaches it with no role context, so that chain crashes at runtime. That is rule 2 below,
and it fires on positive evidence only — a templated include makes edges unknowable, so
"no chain found" proves nothing and stays silent.

## Design

Role context is computed per **chain**, walking the reverse index (T-020) from
`roles:` / `include_role` / `import_role` / meta-dependency sites down include edges.
Context propagates: every file below the role's entry point inherits its role dir. At each
role hop, name → dir uses the existing resolver (post-T-067 order) — no new lookup logic.

- `{{ role_path }}` expands to the **set** of chain-derived role dirs (deduped; usually
  size 1). Missing-file check: try each, a hit on any counts — the `playbook_dir` policy.
  Copy that *policy*, not its **inputs**: `playbook_dir`'s candidate set outside a playbook
  file is still two guesses (`<root>`, `<root>/playbooks`), which T-137 replaces with
  chain-derived dirs using this same walk. Build one, build both.
- Memoize context per file; cycle-guard the walk (T-022 machinery).

Two diagnostics:

1. **missing-file, chain-verified** — the reference resolves under no chain's role dir;
   message lists the chain(s) and paths tried. The warning stays, and can no longer lie.
2. **role_path-outside-role** — the file uses `role_path` and a *known* chain reaches it
   with no role context (e.g. `include_tasks` straight from a play): that chain crashes at
   runtime. Fires on positive evidence only — templated includes make edges unknowable, so
   "no role chain found" proves nothing, and absence of chains stays silent.
3. **end_role-outside-role**, the same walk paying for a second rule. T-110 row 6b ships the
   half that needs no chains — a play's own task list is provably outside every role — but is
   a documented miss in a standalone file, because the caller decides. Measured: a
   byte-identical include target is legal when a role includes it and
   `Cannot execute 'end_role' from outside of a role` when a play does, so the file alone has
   no answer. Same evidence rule as rule 2: fire only when a known chain reaches it with no
   role context. Unlike rule 2 this one is fatal at **load**, not at run time.

## Done when

- [ ] `role_path` expansion uses chain sets; the shape heuristic no longer feeds warnings
- [ ] cross-role include gets the invoker's dir, pinned by fixture
- [ ] rule 2 fires on a play-level `include_tasks` into a `role_path`-using file, and stays
      silent when the only chains are role chains or none are known
- [ ] a live two-chain run (role chain + task chain into the same file) confirms both the
      value and the crash, recorded in Settled
- [ ] rule 3 lifts T-110 row 6b's standalone-file miss, on the same positive-evidence rule

Source: `~/ansible_source/lib/ansible/vars/manager.py:478-484`,
`~/ansible_source/lib/ansible/playbook/task.py:99,495-497`
