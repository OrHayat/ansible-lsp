# T-092 — Includes inside handlers/ resolve against tasks/

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | S    | T-090 | —          |

## Symptom

An `include_tasks:` inside `roles/r/handlers/main.yml` jumps to `roles/r/tasks/<name>.yml`.
Ansible loads `roles/r/handlers/<name>.yml`. Where both exist, go-to-definition opens the
wrong file and the diagnostic vouches for the wrong one.

## Cause

Ansible passes the including file's own subdir as `dirname` — `handlers` for a handler
include, `tasks` for a task include (`playbook/included_file.py:172,199`;
`playbook/helpers.py:166-167`). `FileContext::find_role` (`workspace.rs:223-239`) only ever
produces `role_dir/tasks`, so the handler case is silently mis-based.

## Fix

Carry the including file's role subdir on the context and use it as the first candidate
rather than assuming `tasks`.

## Done when

- [ ] an include inside `handlers/` resolves against `handlers/` first
- [ ] a fixture with the same basename in both dirs pins which one wins
- [ ] role subdirs other than the two are considered rather than assumed away
