# T-097 — include_vars searches paths Ansible never tries

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-090 | T-096      |

## Symptom

`include_vars:` searched a different list of places than Ansible, so it could name a file Ansible
never loads, or miss one it does.

## Measured order

2026-10-21 on 2.21.3 (`scratchpad/t097/order*.py`): a copy of `v.yml` at every candidate, the
winner deleted, the run repeated until it failed. `ansible.cfg` at the repo root, Ansible run
from `/`.

| `include_vars` written in            | Ansible searches, in order                                                         |
| ------------------------------------ | ---------------------------------------------------------------------------------- |
| `roles/r/tasks/main.yml`             | `r/vars`, `r`, `r/tasks/vars`, `r/tasks`, `<playbook dir>/vars`, `<playbook dir>`   |
| `roles/r/tasks/sub/inc.yml`          | `r/vars`, `r`, `r/tasks/sub/vars`, `r/tasks/sub`, `<playbook dir>/vars`, `<playbook dir>` |
| `roles/r/handlers/main.yml`          | `r/vars`, `r`, `r/handlers/vars`, `r/handlers`, `<playbook dir>/vars`, `<playbook dir>` |
| `playbooks/tasks/inc.yml`, no role   | `tasks/vars`, `tasks`, `<playbook dir>/vars`, `<playbook dir>`                       |

The repo root is never searched unless it is the playbook's dir. The ticket's first version
said the role's own `vars/` was not searched: it is searched first (T-206 had already made it so).

## What we searched

`<role>/vars`, `<file dir>/vars`, `<file dir>`, `<project root>/vars`, `<project root>`:

1. **The role root was missing** — second in Ansible's order. Fixed here.
2. **`project_root` stands in for the playbook's dir** — right only when the playbook sits at the
   root, which is T-096's question; this ticket keeps waiting on it for that half.
3. **A `playbooks/` folder with a `tasks/` subfolder is read as a role**, so `playbooks/vars` came
   first for its task files. Filed as T-254.

Corpus count: 26 literal `include_vars` in role task files across 45 public repos and the private
corpus; none lands on a role-root copy, and with the playbook at the root the role root is the only
difference. Scans identical before and after the fix.

## Done when

- [x] the role root is a candidate, after `<role>/vars` — `include_vars_in_a_role_follows_ansibles_measured_order`
  replays all three role orders above
- [x] the templated path searches the same list — `a_substituted_include_vars_searches_the_same_places`;
  `path_bases` kept a copy of its own without the role root
- [ ] the playbook's dir replaces `project_root` (T-096)
- [x] `vars_files` behaviour is unchanged
