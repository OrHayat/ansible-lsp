# T-118 — Collections and routing metadata

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | epic | P2       | L    | —          |

## Problem

Eight tickets, two shipped, all about the same question: **given a short name, which file does
Ansible actually load — and what does the routing table say happened to it?**

They were filed separately over months and read as unrelated. They are not. `T-042` asks
whether the `collections:` keyword is honoured, `T-083` says `ansible.legacy` is the real
entry point for every bare name and that we skip routing for `ansible.builtin`, `T-064` parses
the routing table, `T-119` validates it, and `T-080` proposes rewriting names once we can
resolve them properly. Every one of them needs `PluginLoader`'s order modelled, and none can
be finished with a partial version of it.

The order, for a bare name with no `collections:` (`plugins/loader.py:791`→`841`→
`_find_plugin_legacy`, paths from `:470-521`):

1. `_extra_dirs`, in `add_directory` call order — playbook dir, then `-M`, then each role as
   it loads, dependencies before dependents
2. configured `library` paths, expanded one and two levels deep first
3. `ansible/modules` — always last, "package path always gets added last" (`:496-499`)
4. `_<name>` deprecated-alias retry, then `ansible.builtin.<name>` routing (`:940-959`)

Then routing is consulted **before** the file is checked (`:620` vs `:696`): deprecation →
warning, tombstone → fatal, redirect → follow. A tombstone for a plugin that does not exist
still fires.

The two closed children are the same subsystem's earlier lessons — `T-072` (one platform
action plugin serves a whole network module family) and `T-073` (legacy `action_plugins/`
dirs) — both cases where the naive model was wrong in the same direction.

**T-083 is P1 inside a P2 epic**, for the same reason as T-113: the tool is wrong today
(`yum` → `dnf` redirects are invisible), while the rest is coverage. It should go first.

## Children

- [ ] T-039 — `requirements.yml` ↔ installed-collections cross-check
- [ ] T-042 — Close resolver gaps: collections keyword, collection roles, `*_from`
- [ ] T-064 — Plugin routing: redirects, deprecations, tombstones (+ rename autofix)
- [x] T-072 — Network modules: one platform action plugin handles the whole family
- [x] T-073 — Legacy `action_plugins/` dirs are invisible to the action-plugin check
- [ ] T-080 — Resolution-aware FQCN suggestion that exempts local (`ansible.legacy`) modules
- [ ] T-083 — `ansible.legacy` is unmodelled and `ansible.builtin` skips the routing table
- [ ] T-119 — meta/runtime.yml has no schema validation anywhere

## Done when

- [ ] every child is closed or rejected
- [ ] one module models the loader search order, cited to `plugins/loader.py` and versioned
- [ ] the corpus `module` resolution count (3665 resolved, 0 missing) is unchanged or the
      change is explained
