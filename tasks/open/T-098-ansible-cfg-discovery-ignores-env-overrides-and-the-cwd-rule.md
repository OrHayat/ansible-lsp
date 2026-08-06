# T-098 — ansible.cfg discovery ignores env overrides and the CWD rule

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | S    | T-090 | —          |

## Symptom

We read `roles_path` and `collections_path` from an `ansible.cfg` the user's actual
invocation may be ignoring, and we ignore the environment variables that override it
outright. Every role and collection path downstream inherits the error.

## Cause

`find_ini_config_file` (`config/manager.py:253-313`) is `ANSIBLE_CONFIG` → `./ansible.cfg` in
the **CWD only** → `~/.ansible.cfg` → `/etc/ansible/ansible.cfg`. First hit wins, no merging,
and **no ancestor walk**. `workspace.rs:217-221` walks up from the file.

Also unmodelled: `ANSIBLE_ROLES_PATH`, `ANSIBLE_COLLECTIONS_PATH` and `ANSIBLE_LIBRARY`
override the ini wholesale, while `config.rs` reads only `ANSIBLE_NETWORK_GROUP_MODULES`. And
a world-writable CWD makes its `ansible.cfg` silently skipped (`manager.py:279-284`).

## Approach

Honour the env overrides first, then the ancestor walk. Keep the walk — a repo's config is
what the user means even when they run from a subdirectory — but say in the code that it is
our heuristic and not Ansible's, so the next reader doesn't take it for modelled behaviour.

## Done when

- [ ] `ANSIBLE_CONFIG` is honoured when set
- [ ] the three path env vars override the ini
- [ ] the ancestor walk is commented as an editor heuristic, not Ansible behaviour
- [ ] the scan report says which config file was used
