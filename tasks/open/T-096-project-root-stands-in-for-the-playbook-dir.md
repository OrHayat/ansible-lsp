# T-096 — project_root stands in for the playbook dir

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-090 | T-020      |

## Symptom

We search `<project_root>` for task files. Ansible never looks there — it searches the
**playbook's directory**. Those are the same folder only when the playbook sits at the repo
root, so for the `ansible.cfg`-at-root + playbooks-in-`playbooks/` layout we resolve files
Ansible cannot find, and miss files it can.

## There are three call shapes, not one

Established by **tracing**, not by placing files. `scratchpad/trace_paths.py` wraps
`DataLoader.path_dwim*` and `os.path.exists` at runtime — no edit to the install, nothing to
revert — and prints every candidate probed. Do this first for any future question about
search order; the warning at the end of this section is why.

| Include written in | Call | Candidates |
| ------------------ | ---- | ---------- |
| a **playbook** | `path_dwim(source)` only | **1** — `<playbook_dir>/<source>` |
| a **task file** | `path_dwim_relative(path, dirname=<the include's own subdir>, source)` | ~6, incl. `$CWD/<dirname>/` |
| a **role** | `path_dwim_relative(role_dir, 'tasks', source, is_role=True)` | **7**, incl. `$CWD/tasks/` and `<playbook_dir>/tasks/` |

### All seven, mapped to what we do — the implementation checklist

Traced for `include_tasks: x.yml` in `roles/r1/tasks/main.yml`, so `path=roles/r1`,
`dirname='tasks'`, `basedir=roles/r1`. Source lines are `dataloader.py:288-325`.

| # | Source | Resolves to | Us |
| - | ------ | ----------- | -- |
| 1 | `join(path, dirname, source)` | `roles/r1/tasks/x.yml` | have — `role_anchor_dir` |
| 2 | `unfrackpath(join(basedir, dirname, source))` | same as 1 | have — same |
| 3 | `unfrackpath(join(basedir, 'tasks', source))` — roles only, skipped when `source` already ends in `dirname` | same as 1 | have — same |
| 4 | `unfrackpath(join(dirname, source))` | `$CWD/tasks/x.yml` | **won't model** — upstream accident |
| 5 | `unfrackpath(join(basedir, source))` | `roles/r1/x.yml` | have — `role_dir` |
| 6 | `path_dwim(join(dirname, source))` | `<playbook_dir>/tasks/x.yml` | **MISSING** |
| 7 | `path_dwim(source)` | `<playbook_dir>/x.yml` | approximated by `project_root` |

Seven candidates, five distinct paths — 1, 2 and 3 collapse because `path` is already the
role directory. `basedir` is `unfrackpath(path)`, except that when `is_role` and `path` ends
in `tasks` it becomes `dirname(path)`, which is what makes 5 land on the role root.

**The same seven collapse harder for a task file**, because there `path` *is* the playbook
dir — three distinct locations, not seven:

| Distinct path | From | Us |
| ------------- | ---- | -- |
| `<playbook_dir>/<subdir>/<source>` | 1, 2, 6 | have — `file_dir` |
| `<playbook_dir>/<source>` | 5, 7 | approximated by `project_root` |
| `$CWD/<subdir>/<source>` | 4 | won't model |

`dirname` is **dynamic** — `'tasks'` for a role, the include path's own subdirectory
otherwise. It is not the literal string `tasks`. Assuming it was cost this ticket two wrong
conclusions.

The project root appears in none of the three lists. It resolves only by coincidence, when it
happens to equal `$CWD` or the playbook dir.

**Both remaining candidates need the same missing fact.** #6 and #7 are the two `path_dwim`
entries, and `path_dwim` joins onto `_basedir` — the *invoking playbook's* directory. From a
role or task file that is unknowable without the reverse index, so **the rest of this ticket
is blocked on T-020**, not merely awaiting a corpus scan. #4 we decline regardless.

Approximating #6 as `<project_root>/tasks/<src>` was considered and rejected: it would add a
guessed candidate, and a guess that *widens* resolution manufactures false `Resolved`
verdicts, which is the failure mode this resolver exists to avoid. Better to warn wrongly
about a rare miss than to point confidently at a file Ansible would never load.

## Both of the original claims were right

An earlier revision of this file struck them out. That was wrong, and the tracer proved it.

**The CWD candidate is real.** Same playbook, same files, only the working directory changed:

```
run from /tmp/t096e :  LOADED-VIA-CWD-tasks-x
run from /tmp       :  did not resolve
```

So a `Missing` verdict genuinely cannot be certain for a role or task-file include — that
belongs in a code comment, as the original ticket said. `<playbook_dir>/tasks/<src>` is
likewise a real candidate for role includes.

**That candidate is an upstream accident, not a feature** — see
[`upstream/ansible-cwd-relative-include.md`](../../upstream/ansible-cwd-relative-include.md).
`8f758204cf` (2017) swapped `path_dwim` for `unfrackpath` across the function; on the one
line whose argument was relative that reparented it from the playbook dir to the process
CWD, and the basedir-relative version it duplicates is still six lines below. So we should
**not** model it: cite the dossier and say in the code why a `Missing` here is not provable.

**Why the wrong conclusions happened, so it is not repeated:** black-box placement tests put
files at `$CWD/tasks/` while `dirname` was `'sub'`, so the probes checked paths the algorithm
never builds, and absence was read as proof. Placement tests can *confirm* a candidate; only
tracing can *enumerate* them. Two false corrections reached this ticket before the trace
caught them.

## Fix

**1. Reference in a playbook file — done.** The single candidate is `<playbook_dir>/<source>`,
and in a playbook the playbook dir *is* `file_dir` (T-095), already in our list. So the
`project_root` entry is surplus and drops out — a strict narrowing, no new resolutions.

```
repo/ansible.cfg
repo/playbooks/site.yml   <- include_tasks: helper.yml
repo/helper.yml           <- we used to resolve this; Ansible errors
```

Verified for the exact case changed, with a control so the failure could not be a broken
fixture:

```
playbooks/site.yml, only_at_root.yml at the repo root
  -> Could not find or access '/tmp/t096f/playbooks/only_at_root.yml'
same file moved beside the playbook
  -> LOADED
```

**2. The two candidates we lack** — `<playbook_dir>/tasks/<src>` and `$CWD/<dirname>/<src>`.
Both real, both currently producing false `Missing` warnings. Adding them *widens* what
resolves, so this half needs the corpus scan before it ships. The CWD one also means a
`Missing` here is never provably right, which should be said in the code rather than modelled.

**3. Reference in a standalone task file — needs T-020.** A file that is neither a playbook
nor inside a role, e.g. `demo/tasks/main.yml`:

```
repo/playbooks/site.yml         <- include_tasks: tasks/setup.yml
repo/playbooks/tasks/setup.yml  <- THIS, includes other.yml
```

Its playbook dir is whichever playbook included it — the same reverse-index dependency as
T-137 and T-068. Until then `project_root` stays as the approximation: it is right whenever
the playbook sits at the root, and removing it without a replacement would strand those.

## Rating

Briefly downgraded to P2/S on the strength of the two wrong conclusions; restored to **P1/M**
once tracing showed both original claims stood. We miss two real candidate locations (false
`Missing`) and search one Ansible never uses (false `Resolved`). Wrong in both directions is
what P1 is for.

## Done when

- [x] a reference in a playbook file no longer searches `project_root` — `task_bases`
- [x] a fixture pins both directions — `a_playbook_does_not_search_the_project_root`, which
      also pins the playbook-at-the-root case, where `file_dir` and `project_root` dedupe to
      one entry and a naive filter would have deleted the file's own directory
- [x] roles keep every slot they have today
- [x] case 3 is left to T-020, said so in the code
- [ ] `<playbook_dir>/tasks/<src>` is added as a candidate (real, and intended upstream)
- [ ] `$CWD/<dirname>/<src>` is **not** modelled, and the code says why, citing
      `upstream/ansible-cwd-relative-include.md`
- [ ] the corpus scan count is unchanged — **not run**, `~/app/ansible` is absent from this
      machine. The shipped half is a narrowing and cannot add resolutions; the remaining half
      can, so it must not ship without the scan.
