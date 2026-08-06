# T-042 — Close resolver gaps: collections keyword, collection roles, `*_from`

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P2       | S    | T-118 | —          |

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

### 4. Module name shapes, live-verified 2026-08-03 (2.21)

- `debug` → **works** (implicit `ansible.legacy.debug`, falls through to builtin via the
  routing table — the bare-name box in T-064)
- `builtin.debug` → **fails**: "Cannot resolve 'builtin.debug' to an action or module."
- `ansible.builtin.debug` → works

~~The resolver skips everything that isn't 3 parts~~ — fixed; the resolver now walks the
loader's own candidate order for a bare `foo:` (transcribed from
`loader.py:470-521,956-959`, documented on `resolve_module`):

```
1. <role>/library/foo.py                 extra_dirs — local overrides shipped
2. <file dir>/library/foo.py
3. <project>/library/foo.py
4. cfg `library` key dirs, else ~/.ansible/plugins/modules + /usr/share/ansible/plugins/modules
5. <install>/ansible/modules/foo.py      package always last (loader.py:497)
6. <install>/ansible/plugins/action/foo.py
7. all missed → rename table (core's, then each collection's own meta/runtime.yml) gives
   a new FQCN → restart under that name, visited-set cycle guard  → T-064
```

First existing file wins at every level, same as the loader. 2-part names — a *statically
provable* runtime failure, since module names can't contain dots so only 1 or 3 parts are
possible — still stay silent where an ERROR quoting Ansible's message is safe.

## Done when

- [ ] a short module name resolves through an in-scope `collections:` list (or a test proves it
      already does)
- [ ] `ns.coll.role` resolves from `collections_path` (or a test proves it already does)
- [x] a bare builtin name (`debug:`) resolves and hovers like its FQCN — bare names now
      extract and resolve in the loader's order (workspace `library/` shadowing pinned by
      `demo/library/ping.py`; order documented on `resolve_module_bare`), including the
      split-table redirect from `ansible_builtin_runtime.yml`. Corpus: module resolutions
      3665 → 5894, still 0 missing. Redirect chains and per-collection tables stay T-064.
- [ ] a 2-part name (`builtin.debug`) gets an ERROR quoting "Cannot resolve … to an
      action or module", pinned by fixture

Docs: https://docs.ansible.com/ansible/latest/collections_guide/collections_using_playbooks.html
