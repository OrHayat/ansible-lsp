# T-238 — A module in a collection subdirectory is never extracted, so its FQCN gets no hover, jump or diagnostic

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P2       | M    | T-118 | —          |

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

## Resolution

Measured on 2.21.3, collection `demo.probe` beside the playbook:

| written                                | Ansible                           | us now                  |
| -------------------------------------- | --------------------------------- | ----------------------- |
| `demo.probe.sub.deeper.deepest`        | ran `sub/deeper/deepest.py`       | resolved                |
| `demo.probe.sub.act`, action plugin only | ran `plugins/action/sub/act.py` | resolved                |
| `demo.probe.sub.alias`, routed under the key `sub.alias` | ran `sub/deep.py` | resolved, via the redirect |
| `demo.probe.sub.deep.extra`            | did not resolve                   | skipped                 |
| `ansible.builtin.ping.ping`            | **ran `ping`**                    | skipped                 |
| `ansible.builtin.wait_for.nothere`, `ansible.legacy.ping.ping` | did not resolve | skipped |

The `ansible.builtin.X.X` row turned up in the corpus check: oneview writes
`ansible.builtin.wait_for.wait_for:`, and it runs. How core resolves that is not modelled, so every
nested `ansible.*` name stays skipped, which is silent rather than wrong. It was the only new
reference across 46 repos, and no corpus scan changed otherwise.

The hover's collection arm (`not_followed_hover`) matched three parts only, so an unresolved nested
name would have said "not in ansible-core, nor in any `library` dir". It now takes any depth.
Demo: `demo/nested_modules.yml`, with `net/port_probe.py` in the demo's `demo.charlie`.

## Done when

- [x] `demo.probe.sub.deep` and `demo.probe.sub.deeper.deepest` resolve to their files, in-memory
      fixture, with a control that `demo.probe.deep` does not —
      `a_module_in_a_collection_subdirectory_resolves_by_its_full_dotted_path`
- [x] the `collections:` list does not reach a nested module (`sub.deep`, `deep`), pinned — same test
- [x] routing and action-plugin behaviour for nested names measured and pinned, or recorded here
      as not applicable — both apply, see Resolution
- [x] an unresolved nested name hovers T-133's collection wording and is not a diagnostic —
      `a_reference_we_did_not_follow_says_why`, plus `the_nested_modules_demo_matches_its_annotations`

## Config settings that change this (T-144 audit, ansible-core 2.21.2)

Precedence is env -> ini -> default; none of these declare a `vars`, `cli` or
`keyword` rung, so those do not apply (the T-098 pattern).

| setting | env | ini | what it changes |
| --- | --- | --- | --- |
| `MODULE_IGNORE_EXTS` | `ANSIBLE_MODULE_IGNORE_EXTS` | `[defaults] module_ignore_exts` | List of extensions to ignore when looking for modules to load. This is for rejecting script and binary module |
