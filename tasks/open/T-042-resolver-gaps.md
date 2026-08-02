# T-042 — Close resolver gaps: collections keyword, collection roles, `*_from`

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | S    | —          |

## Problem

Three small holes in the *existing* reference resolver, found while surveying import-shaped
mechanisms. Each is a verify-and-maybe-extend, not a new subsystem.

### 1. `collections:` keyword affects short module names

```yaml
- hosts: all
  collections: [community.docker]
  tasks:
    - docker_container: ...   # short name, resolves via the collections: search list
```

Module navigation (T-005) resolves FQCNs — verify whether it honours a play/role
`collections:` list when resolving a *short* module name. If not, that's a false "unknown
module" on a common pattern.

### 2. Collection-hosted roles (`namespace.collection.role`)

```yaml
- include_role: { name: my_ns.my_coll.setup }
```

Verify the role resolver searches `collections_path` for `ns.coll.role`, not just
`roles_path`. If it only checks `roles_path`, collection roles read as missing.

### 3. ~~`vars_from:` / `defaults_from:` / `handlers_from:`~~ → moved to T-063

The "identical shape, trivial to add" framing was wrong — `_load_role_yaml` has per-case
extension order, dir forms, and hard-error semantics. The full include_role parameter
surface (these three included) is now **T-063**; this ticket keeps only the two verify
items above.

## Done when

- [ ] a short module name resolves through an in-scope `collections:` list (or a test proves it
      already does)
- [ ] `ns.coll.role` resolves from `collections_path` (or a test proves it already does)

Docs: https://docs.ansible.com/ansible/latest/collections_guide/collections_using_playbooks.html
