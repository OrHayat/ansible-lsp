# Upstream issue to file against ansible/ansible — include resolution depends on the caller's CWD

Not filed yet. Read from the checkout at `~/ansible_source`, behaviour verified live against
ansible-core **2.21.2** under WSL.

An `include_tasks:` inside a role or a task file can resolve — or not — depending on the
directory the operator happened to `cd` into before running `ansible-playbook`. Nothing in
the repository determines it. It appears to be an accidental reparenting introduced by a
2017 cleanup, not a designed lookup.

---

## Symptom

Same playbook, same files on disk, only the working directory changed:

```
$ cd /tmp/t096e && ansible-playbook -i localhost, -c local playbooks/site.yml
    "msg": "LOADED-VIA-CWD-tasks-x"

$ cd /tmp      && ansible-playbook -i localhost, -c local /tmp/t096e/playbooks/site.yml
    (does not resolve)
```

Fixture: `roles/r1/tasks/main.yml` contains `include_tasks: x.yml`; the only copy of `x.yml`
is at `/tmp/t096e/tasks/x.yml` — a `tasks/` directory at the project root, part of no role.
It is reachable only because the first run's CWD happened to be its parent.

## Cause

`DataLoader.path_dwim_relative` (`lib/ansible/parsing/dataloader.py:279-331`) builds its
candidate list. One entry omits the base directory:

```python
# try to create absolute path for loader basedir + templates/files/vars + filename
search.append(unfrackpath(os.path.join(dirname, source), follow=False))
```

`os.path.join(dirname, source)` is **relative**, and `unfrackpath` ends in `os.path.abspath`,
which resolves against the process's current working directory. So this candidate is
`<cwd>/<dirname>/<source>` — for a role, `<cwd>/tasks/<source>`.

Two things show this is unintended rather than a feature:

1. **The comment above it still describes the old behaviour** — "loader basedir + … +
   filename" — while the code no longer applies the basedir.
2. **The intended candidate still exists six lines below**, unchanged:
   ```python
   # try to create absolute path for  dirname + filename
   search.append(self.path_dwim(os.path.join(dirname, source)))
   ```
   `path_dwim` joins onto `self._basedir`. So the list currently contains the same candidate
   twice — once basedir-relative as designed, once CWD-relative. A deliberate CWD lookup
   would not be a duplicate of an entry already present.

## Origin

`8f758204cf` — Brian Coca, 2017-07-03, "correct, cleanup & simplify dwim stack" (#25956).
The commit replaces `self.path_dwim(...)` with `unfrackpath(...)` throughout the function:

```diff
-            search.append(self.path_dwim(os.path.join(basedir, dirname, source)))
+            search.append(unfrackpath(os.path.join(basedir, dirname, source), follow=False))
-                search.append(self.path_dwim(os.path.join(basedir, 'tasks', source)))
+                search.append(unfrackpath(os.path.join(basedir, 'tasks', source), follow=False))
-            search.append(self.path_dwim(os.path.join(dirname, source)))
+            search.append(unfrackpath(os.path.join(dirname, source), follow=False))
```

The first two arguments are already absolute (`basedir` is prefixed), so for those the swap
is behaviour-preserving — `path_dwim` on an absolute path only normalises, exactly as
`unfrackpath` does. The third argument is relative, and there the same edit silently changed
the base from the playbook directory to the process CWD. One mechanical substitution, one
line where it mattered.

## Traced candidate list

`scratchpad/trace_paths.py` (runtime monkeypatch of `DataLoader.path_dwim*` and
`os.path.exists`; nothing on disk is modified). For `include_tasks: x.yml` in
`roles/r1/tasks/main.yml`, with the project at `/tmp/t096e` and CWD `/tmp/t096e`:

```
path_dwim_relative(path='/tmp/t096e/roles/r1', dirname='tasks', source='x.yml', is_role=True)
  probe  /tmp/t096e/roles/r1/tasks/x.yml      (candidates 1-3 collapse here)
  probe  /tmp/t096e/tasks/x.yml               <- CWD-relative
  probe  /tmp/t096e/roles/r1/x.yml            role root
  probe  /tmp/t096e/playbooks/tasks/x.yml     path_dwim(dirname/source)
  probe  /tmp/t096e/playbooks/x.yml           path_dwim(source)
```

Row 2 and row 4 are the duplicate pair — the same expression, one missing its base.

## Impact

Low frequency, high confusion. It needs a directory named like the include's `dirname`
(`tasks/` for roles) to exist under the CWD *and* the file to be missing from every earlier
candidate. When it does fire, a playbook works from one directory and fails from another with
no repository-visible cause, and CI — which usually runs from the repo root — can disagree
with a developer's shell.

## Suggested fix

Delete the CWD-relative entry. The basedir-relative one it duplicates is already in the list,
so nothing that resolves for a legitimate reason stops resolving. If the duplicate is instead
considered load-bearing by now, at minimum correct the comment so it describes what the line
does.

## Why this repo cares

`resolve.rs` cannot model a candidate that depends on the operator's shell, so a `Missing`
verdict on a role or task-file include can never be *provably* right. T-096 cites this
dossier as the reason it documents that limit in a comment rather than attempting to
reproduce the candidate.
