# T-067 — Role search order doesn't match Ansible's

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P1       | M    | —          |

## Problem

`roles_roots()` (`workspace.rs:55`) adds "the current role's parent" as a roles root for
*every* file. Real Ansible's search for a play-level `roles:` entry
(`definition.py:164-188`) is:

1. `<playbook_dir>/roles`
2. configured `roles_path` (replaces the defaults — we already model that)
3. `role_basedir` — **only** while loading a role's dependencies
4. `<playbook_dir>` itself
5. last resort: the role *name* as a path, relative to the process CWD
   (`definition.py:190-194`)

The parent-as-root rule is an over-approximation of item 3: Ansible has it only in
dependency context, we apply it everywhere. Live-verified consequence: `demo/playbook.yml`'s
`roles: - demo` resolves in our resolver but hard-errors in real Ansible when run from
`demo/` (`The role 'demo' was not found`). From the repo root it resolves — but only via
item 5, the CWD fallback, which no static tool can know. The tool goes silent on a break
Ansible reports, which is this board's definition of P1.

Item 5 is unknowable statically. Policy: a role reachable *only* through it stays
unresolved, and the missing-role diagnostic's candidate list may note the fallback path
that would need a specific launch directory.

## Done when

- [ ] play-level role references search exactly Ansible's list (1, 2, 4), pinned by fixture
- [ ] parent-as-root survives only in meta-dependency resolution, pinned by fixture
- [ ] `roles: - demo` in the demo warns missing-role, matching the live run
- [ ] a live `ansible-playbook` run per root verifies the order, recorded in Settled if it
      contradicts current tests

Source: `~/ansible_source/lib/ansible/playbook/role/definition.py:131-198`
