# T-238 — A module in a collection subdirectory is never extracted, so its FQCN gets no hover, jump or diagnostic

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-118 | —          |

## Problem

A collection may nest modules under `plugins/modules/`, and Ansible addresses them by the full
dotted path. Measured 2026-09-17 on 2.21.3, collection `demo.probe` with
`plugins/modules/sub/deep.py` and `plugins/modules/sub/deeper/deepest.py`:

| written                                      | Ansible                            |
| -------------------------------------------- | ---------------------------------- |
| `demo.probe.sub.deep`                        | ran `sub/deep.py`                  |
| `demo.probe.sub.deeper.deepest`              | ran `sub/deeper/deepest.py`        |
| `demo.probe.deep`                            | `couldn't resolve module/action`   |
| `sub.deep` under `collections: [demo.probe]` | `couldn't resolve module/action`   |
| `deep` under `collections: [demo.probe]`     | `couldn't resolve module/action`   |
| `ansible.legacy.sub.deep`                    | `couldn't resolve module/action`   |

So the only spelling is the full FQCN, at any depth: no flattening, and the `collections:` list
does not reach a subdirectory.

We extract a module reference only when the name has at most three dotted parts
(`references.rs`, `dots <= 3`), so a 4+ part name produces no reference at all — no hover, no
go-to-definition, and no diagnostic when the file is absent. Silent rather than wrong: nothing
claims anything about it. (`resolve_module` also skips any 4+ part name, `resolve.rs`, but no
reference reaches that arm today.)

## Approach

Extract 4+ part names as module references, and resolve `ns.coll.a.b.module` to
`<root>/ns/coll/plugins/modules/a/b/module.*` (and `plugins/action/a/b/module.py`), through the
same candidate builder the 3-part arm uses. Measure before asserting:

- whether a collection's `meta/runtime.yml` can route a dotted subdirectory name, and under
  which key spelling
- whether `plugins/action/<sub>/` is searched for a nested name the way `plugins/modules/` is
- what an unresolved nested name should do: it is the 3-part case's twin (usually a collection
  not installed here), so skipped with T-133's hover, not warned

`sub.deep` is two parts and already the `invalid-module-name` ERROR, which the table above
confirms is right.

## Done when

- [ ] `demo.probe.sub.deep` and `demo.probe.sub.deeper.deepest` resolve to their files, in-memory
      fixture, with a control that `demo.probe.deep` does not
- [ ] the `collections:` list does not reach a nested module (`sub.deep`, `deep`), pinned
- [ ] routing and action-plugin behaviour for nested names measured and pinned, or recorded here
      as not applicable
- [ ] an unresolved nested name hovers T-133's collection wording and is not a diagnostic
