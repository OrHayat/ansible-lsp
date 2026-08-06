# T-091 — with_ext misses .json and extensionless, and tasks_from flips the order

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | S    | T-090 | —          |

## Symptom

A role file Ansible loads is reported missing, or we jump to one Ansible would not have
loaded. `tasks_from: setup` against `roles/r/tasks/setup` (no extension) or `tasks/setup.json`
resolves for Ansible and not for us. Where both `tasks/main.yml` and `tasks/main.yaml` exist
only the `.yml` is live, and we do not say so.

## Cause

`resolve.rs:663-671` (`with_ext`) tries `.yml` then `.yaml`. `Role._load_role_yaml` hardcodes
`['.yml', '.yaml', '.json']` (`role/__init__.py:421-422`) — deliberately *not*
`C.YAML_FILENAME_EXTENSIONS`, "to maintain portability" — and then:

- default entry point (`main`): appends `''` **last** → `main.yml`, `main.yaml`, `main.json`, `main`
- with any `*_from:`: inserts `''` **first** (`:429-431`) → the literal name given wins

`DataLoader.find_vars_files` breaks on the first hit (`parsing/dataloader.py:491`), so the
loser is silently dead rather than merged.

## Fix

Give `with_ext` the real extension list plus a flag for which end `''` goes on, driven by
whether the reference carried a `*_from`. Same for the `tasks/main` probe at `resolve.rs:440`.

## Done when

- [ ] `.json` and the extensionless form resolve
- [ ] `tasks_from: setup` prefers `tasks/setup` over `tasks/setup.yml`
- [ ] a test pins both orders against one fixture tree
- [ ] first-hit-wins is modelled, so a shadowed `main.yaml` is not reported as live
