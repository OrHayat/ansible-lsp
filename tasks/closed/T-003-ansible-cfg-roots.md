# T-003 — `ansible.cfg` roots

| Status | Priority | Size | Commit  |
| ------ | -------- | ---- | ------- |
| done   | P2       | S    | d722abb |

## Problem

Roles are not required to live in `./roles`. The real repo's config says
`roles_path = ~/ansible/roles:./roles`, so a hardcoded root — what the legacy plugin does —
cannot find half of them. `include_role` can't resolve without this, so it shipped alongside
T-004.

## Outcome

`config.rs` parses the nearest ancestor `ansible.cfg` (INI) for `roles_path` and
`collections_path`: `~`-expanded, `:`-split, and resolved against the **config file's own**
directory rather than the process cwd. Falls back to conventions when absent.

`install.rs` locates the installed Ansible so `ansible.builtin` and installed collections
resolve too — resolving `which ansible` and looking for `lib/python*/site-packages/ansible`,
with `ansible --version` only as a fallback. Cached in a `OnceLock`.

`~/ansible/roles` is on `roles_path` but outside the workspace: targets there must be
navigable, but it is excluded from the repo-wide scan.
