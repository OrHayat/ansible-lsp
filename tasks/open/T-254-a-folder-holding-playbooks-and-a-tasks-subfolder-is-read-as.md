# T-254 — A folder holding playbooks and a tasks/ subfolder is read as a role

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P3       | M    | T-090 | —          |

## Symptom

`workspace::is_role_dir` calls any folder with a `tasks/`, `defaults/` or `meta/` subfolder a
role, and `FileContext` then treats every file under it as a role file. A `playbooks/` folder
that keeps its included task files in `playbooks/tasks/` is the common case.

Measured 2026-10-21 on 2.21.3 (`scratchpad/t097/order.py taskfile`): `playbooks/site.yml` includes
`tasks/inc.yml`, which does `include_vars: v.yml`, with a copy at every candidate. Ansible
searches `playbooks/tasks/vars`, `playbooks/tasks`, `playbooks/vars`, `playbooks`. We search
`playbooks/vars` **first**, as that "role's" `vars/`, so where both exist we name the wrong file.

`include_vars` is where it was measured, not where it ends: every reader of `role_dir` and
`role_anchor_dir` sees a role here — task include search, `role_path`, the role-escape hint
(`role_escape.rs` already works around it by requiring a `roles/` parent).

## Cause

`is_role_dir` asks what a folder contains, not whether anything loads it as a role.

## Fix

Not decided. Candidates: a role is a folder under a `roles/` directory, on `roles_path`, or in
a collection's `roles/`, or one a `roles:`/`include_role` reference resolves to (T-020) — and
measure what each costs on the corpora, since a role repo checked out at the workspace root has
none of the first three.

## Done when

- [ ] `playbooks/tasks/inc.yml`'s `include_vars` searches in Ansible's measured order, pinned by
  a test
- [ ] every `role_dir` reader checked against a folder that only looks like a role
