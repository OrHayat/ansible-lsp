# T-092 — Includes inside handlers/ resolve against tasks/

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | S    | T-090 | —          |

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

Done: `FileContext::role_tasks_dir` became `role_anchor_dir` — `find_role` keys on the
file's actual subdir (`tasks` or `handlers`, the two that hold task lists; upstream itself
is a hardcoded two-way on `isinstance(original_task, Handler)`, `included_file.py:172`).
`task_search_dirs` puts the anchor first and, for a handler file, the role's `tasks/`
second — the legal fallback from `path_dwim_relative`'s "look in role's tasks dir w/o
dirname" (`dataloader.py:311-313`) — so a handlers-only miss doesn't invent a warning.
Demo: `demo/roles/notifier` has `restart.yml` in both subdirs (the handler include must
open the `handlers/` one) plus a `shared.yml` reachable only via the fallback.

## Done when

- [x] an include inside `handlers/` resolves against `handlers/` first
- [x] a fixture with the same basename in both dirs pins which one wins
- [x] role subdirs other than the two are considered rather than assumed away
