# T-097 — include_vars searches paths Ansible never tries

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-090 | T-096      |

## Symptom

`include_vars:` reports resolved for files Ansible would never find, and missing for files it
would. The false positives are the worse half — we vouch for a path that fails at runtime.

## Cause

Ansible resolves via `_find_needle('vars', src)` → `path_dwim_relative_stack` over
`task.get_search_path()` (`plugins/action/__init__.py:1540-1551`; `playbook/base.py:767-779`).
For a task in `roles/r/tasks/main.yml` that is:

```
roles/r/tasks/vars/<src>
roles/r/tasks/<src>
<playbook dir>/vars/<src>
<playbook dir>/<src>
```

The role's own `vars/` is **not** searched: the role branch at `dataloader.py:365-368`
requires `dirname(unfrackpath(path)).endswith('/tasks')`, never true for a search-path entry
that is itself a directory.

`resolve.rs:357-369` uses `[file_dir, file_dir/vars, role_dir/vars, project_root]` —
`role_dir/vars` and `project_root` are candidates Ansible never tries, and both playbook-dir
entries are missing.

`vars_files` uses the same function with `play.get_search_path()`, and
`vars_files_candidates` (`resolve.rs:644-661`) already matches it. Only `include_vars` is wrong.

## Approach

Build the candidate list from the real search path. Needs T-096 for the playbook dir.

## Done when

- [ ] `role_dir/vars` and `project_root` are gone from the `include_vars` candidates
- [ ] both playbook-dir candidates are present
- [ ] a fixture pins that a file in the role's own `vars/` does *not* resolve
- [ ] `vars_files` behaviour is unchanged
