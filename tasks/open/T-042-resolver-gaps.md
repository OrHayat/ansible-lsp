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

## Measured, 2026-09-12, ansible-core 2.21.2

Every row below was run, not read. Probes: `scratchpad/t042_collections_keyword_probe.sh`
(A–G), `t042_collections_in_role_probe.sh` (H–J), `t042_collections_order_probe.sh` (K–P),
`t042_collection_roles_probe.sh` (Q–U), `t042_fallthrough_probe.sh` (V–W),
`t042_two_part_probe.sh` (X1–X4). Each fixture builds two collections that ship the *same*
module name, so the losing candidate exists and the probe can come out the other way.

`collections:` is a **search list**, not a fact: a short name is tried as `<entry>.<name>`
for each entry in order, and only then as `ansible.legacy.<name>`.

| Row | Setup | Ran |
| --- | ----- | --- |
| A  | `probe_mod:`, no list | `couldn't resolve module/action 'probe_mod'` |
| B  | `probe_mod:` + `collections: [ns.coll]` | `ns.coll.probe_mod` |
| C  | `ns.coll.probe_mod:`, no list | resolved — an FQCN never needs the list |
| D  | `ping:`, no list, `library/ping.py` present | `library/` ping |
| E  | `ping:` + `collections: [ns.coll]`, `library/ping.py` present | **`ns.coll.ping`** |
| F  | `ansible.builtin.ping:` + `collections: [ns.coll]` | builtin — an FQCN ignores the list |
| G  | E without `library/` | `ns.coll.ping` |
| K1 | `collections: [ns.a, ns.b]`, both ship `dup` | `ns.a.dup` |
| K2 | reversed to `[ns.b, ns.a]` | `ns.b.dup` — order is the rule |
| L  | `debug:` + `collections: [ns.a]` (no `debug` there) | builtin `debug` |
| M  | **task**-level `collections: [ns.b]` | `ns.b.only_b` |
| N  | **block**-level `collections: [ns.b]` | `ns.b.only_b` |
| O  | `ns.b.dup:` under `collections: [ns.a]` | `ns.b.dup` |
| P  | `only_b:` under `collections: [ns.a]` | `couldn't resolve module/action 'only_b'` |
| V  | `ping:` + `collections: [ns.a]` (no `ping` there), `library/ping.py` present | `library/` ping |
| W  | V without `library/` | `ansible.builtin.ping` |

So the order is: **every list entry, in order → `ansible.legacy` (`library/`, then the
builtin package)**. The list goes *ahead* of the workspace's own `library/` (row E), which
is what makes it more than a fallback.

### Scope: the list does not flow the way a variable does

| Row | Setup | Ran |
| --- | ----- | --- |
| H | role task `probe_mod:`, no list anywhere | fails |
| I | role's own `meta/main.yml` has `collections: [ns.coll]` | `ns.coll.probe_mod` |
| J | **play** has the list, the role it calls has none | **fails** |

A play's list does not reach into a role it calls — the role carries its own, in
`meta/main.yml`. Any implementation that indexes the lists project-wide gets row J wrong.

### Roles (item 2)

| Row | Setup | Ran |
| --- | ----- | --- |
| Q | `include_role: {name: ns.coll.setup}`, no list | resolved from the collection |
| R | `roles: [ns.coll.setup]`, no list | resolved from the collection |
| S | `include_role: {name: setup}`, no list | `The role 'setup' was not found in: …` |
| T | `include_role: {name: setup}` + `collections: [ns.coll]` | **`ns.coll.setup`** |
| U | `roles: [local_only]` under a list, role only in `roles/` | `roles/local_only` |

Q and R already work — `role_dir` searches `collection_roots()` for a 3-part name. T does
not: the list applies to **roles as well as modules**, which the item as filed did not say.

### The message item 4 quotes is the wrong one

Two different errors exist, and which one fires depends on the task's shape:

| Row | Shape | Error |
| --- | ----- | ----- |
| X1 | `- builtin.debug: {msg: x}` | `couldn't resolve module/action 'builtin.debug'…` (`mod_args.py:364`) |
| X2 | `- action: {module: builtin.debug}` | `Cannot resolve 'builtin.debug' to an action or module.` (`task.py:195`) |
| X3 | `- action: "{{ m }}"` → `builtin.debug` | same as X2 |
| X4 | `- nosuchmodule:` | same as X1 |

The ordinary module-as-key shape dies at **parse** time with the `mod_args` message — the
play never starts — and produces the *same* text as an ordinary unknown module (X1 vs X4),
so a 2-part name is not distinguishable from a typo by message alone. The `task.py` text
this ticket quotes only fires for the `action:` spellings. Item 4's wording needs fixing
before anyone writes that diagnostic.

### How the scopes combine — measured second, because the first round only ever set one

`scratchpad/t042_nesting_probe.sh` (AA–AG), `t042_merge_probe.sh` (BA–BD),
`t042_cross_file_probe.sh` (CA–CC), `t042_templated_entry_probe.sh` (DA–DB).

| Row | Setup | Ran |
| --- | ----- | --- |
| AA | play `[ns.a]` + task `[ns.b]`, both ship `dup` | `ns.b.dup` |
| AB | play `[ns.a]` + block `[ns.b]` | `ns.b.dup` |
| AD | role meta `[ns.a]` + task-in-role `[ns.b]` | `ns.b.dup` |
| AE | role meta `[ns.a]`, task writes none | `ns.a.dup` |
| BA | play `[ns.a]` + task `[ns.b]`, module **only** in `ns.a` | **fails** |
| BB | same, module only in `ns.b` | `ns.b.only_b` |
| BC | play `[ns.a]` + block `[ns.b]`, module only in `ns.a` | **fails** |
| BD | role meta `[ns.a]` + task-in-role `[ns.b]`, module only in `ns.a` | **fails** |
| DA | play `[ns.a]` + task `["{{ c }}"]` with `c: ns.a` set, module in `ns.a` | **fails** |
| DB | same without the task list | `ns.a.only_a` |

**An inner list replaces the outer one; it never extends it.** AA alone could not show that
— both collections shipped the module, so "nearest wins" and "both searched, inner first"
give the same answer. BA is the row that separates them, and it fails.

DA is the same rule with a dead entry: `collections` is `static=True` upstream, so `{{ c }}`
is never rendered and matches nothing — but it still replaces the play's list. That is why
[`crate::ast::collections_of`] keeps templated entries instead of dropping them: an empty
list is how "inherit" is spelled, so dropping them would silently hand the task its play's
search list.

| Row | Setup | Ran |
| --- | ----- | --- |
| AF | `include_role: {name: shared}` + `[ns.coll]`, `roles/shared` **and** `ns.coll.shared` exist | `ns.coll.shared` |
| AG | `roles: [shared]`, same collision | `ns.coll.shared` |

For a role name the list beats `roles_path`, exactly as it beats `library/` for a module.

### The one that does not fit per-file resolution

| Row | Setup | Ran |
| --- | ----- | --- |
| CA | `include_tasks: inc.yml` under a play list | `ns.a.only_a` |
| CB | `import_tasks: inc.yml` under a play list | `ns.a.only_a` |
| CC | same, no list | fails |

A play's list **does** cross into a plain task file, unlike into a role (row J). We resolve
one file at a time and `inc.yml` holds no play, so the list is the caller's and not the
file's. Left unfixed, pinned by the ignored
`a_plays_collections_list_reaches_a_file_it_includes` — it wants the invocation chain, the
same thing [[T-068]] needs.

### Item 4, measured properly and shipped

`scratchpad/t042_name_shapes_probe.sh` (which shapes are legal) and
`t042_two_part_tier_probe.sh` (when each one fails).

| Name | Parts | 2.21.2 |
| ---- | ----- | ------ |
| `debug` | 1 | runs |
| `ansible.builtin.debug` | 3 | runs |
| `ansible.legacy.debug` | 3 | runs |
| `builtin.debug` | 2 | `couldn't resolve module/action 'builtin.debug'` |
| `ansible.debug` | 2 | same |
| `legacy.debug` | 2 | same |
| `ansible.builtin.nosuch` | 3 | **same message** |

The last row is the one that shapes the rule. The message does not distinguish "impossible
shape" from "collection not installed here", so it cannot be the thing we key on. What can
is the shape itself: a collection is always `namespace.name`, so a qualified module has
three parts and a short one has one, and a module name cannot contain a dot — two parts has
no third reading. **A two-part name is wrong whatever is installed**, which is exactly why
it can be reported when an unresolved three-part name still cannot be.

| Row | Spelling | First task before it |
| --- | -------- | -------------------- |
| EA | `- builtin.debug:` | **did not run** — `ModuleArgsParser`, parse time, the play never starts |
| EB | `- action: {module: builtin.debug}` | ran — `Task._post_validate_args`, run time |
| EC | `- local_action: builtin.debug` | ran — same |

Two different failures, so two different messages. `ast::Action::from_action_keyword` and
`Reference::action_keyword` carry which spelling was written; without them the diagnostic
would quote one failure while describing the other, which is the mistake this ticket's own
box made for two years.

`impossible_module_name` is the shared predicate (on the data, per rule 3): the extractor
uses it to decide the name is worth a reference, `resolve_module` to return `Missing`
rather than `Skipped`, and `rule_id` to hand back `invalid-module-name`. Severity is ERROR
— the play does not start, or the task cannot run; neither is the "ansible skips it and
carries on" tier a missing file gets. Suppressible by its own id.

The old `assert!(out.iter().all(|(r, _)| r.kind != ReferenceKind::Module))` in
`bare_module_names_resolve_in_the_loaders_order` is gone: it pinned the gap, and it sat
*after* that test's `package_dir.is_none()` early return, so on a machine with no Ansible
install it never ran at all.

| Test | Covers |
| ---- | ------ |
| `a_two_part_module_name_is_missing_with_its_own_rule` | all three 2-part spellings → `Missing` + the rule id + no candidates; controls: 1-part and an uninstalled 3-part stay quiet |
| `the_action_keyword_spelling_is_recorded_on_the_reference` | EA/EB/EC's flag survives extraction |
| `a_two_part_module_name_is_an_error_quoting_its_own_failure` (lsp) | ERROR severity, the right quote per spelling, the two messages differ, both controls quiet, `# noqa` works |

Each was confirmed red under a break of what it covers: the predicate, the extraction, the
severity, and the spelling flag. The demo scan is byte-identical — no two-part name exists
in `demo/`, so the rule fires nowhere yet.

## What landed

- `ast::Play`/`Block`/`Task` gained `collections: Vec<String>` (`collections_of`), templated
  entries kept.
- `references`: `Reference::collections` carries the **nearest** list, threaded play →
  block → task by `nearest()`, and onto `roles:` entries from the play (row AG).
- `resolve`: `collections_in_scope` adds the role `meta/main.yml` fallback for a file inside
  a role — read only for a bare name, since an FQCN ignores the list. `fqcn_module_candidates`
  was lifted out of the FQCN arm so a list entry builds the same candidates for
  `<entry>.<bare>`; the bare arm runs the list first, then `ansible.legacy`. `role_dir` does
  the same for a short role name before `roles_path`.

Demo scan is verdict-identical before and after — the only difference is 153 → 159 file
reads, the `meta/main.yml` lookups. A fixture on disk (`collections/`, `library/ping.py`,
a role with a meta list, one playbook with a list and one without) scans to 4 modules
resolved / 2 skipped and the role resolved from the collection, which is what ansible-core
did with the same tree.

## Tests

All of the above are pinned in `crates/ansible-core/src/resolve.rs`, sharing a
`collections_fs()` fixture. All but one are live. They were written **before** the fix, six
of them carrying `#[ignore = "asserts the collections:-list answer we do not give yet —
T-042"]`, and each was confirmed to fail then for the right reason (`Skipped`/`[]`/the
legacy path, never a panic) before the attribute came off. Every test here was also
confirmed red under a deliberate break of the code it covers — the list lookup in the bare
module arm, the one in `role_dir`, `nearest()` inheriting instead of replacing, and the
role `meta/main.yml` read.

| Test | Rows | State |
| ---- | ---- | ----- |
| `a_short_module_name_no_list_reaches_resolves_nowhere` | A, P | live |
| `a_short_module_name_resolves_through_the_plays_collections_list` | B | ignored |
| `the_collections_list_is_searched_in_order` | K1, K2 | ignored |
| `a_listed_collection_shadows_the_workspace_library` | E | ignored |
| `an_unlisted_name_still_falls_through_to_ansible_legacy` | V, W | live |
| `an_fqcn_ignores_the_collections_list` | C, F, O | live |
| `collections_on_a_block_or_a_task_applies_to_that_scope` | M, N | ignored |
| `a_roles_own_meta_collections_list_applies_to_its_tasks` | I | ignored |
| `a_plays_collections_list_does_not_reach_a_role_it_calls` | H, J | live |
| `a_collection_hosted_role_resolves_by_fqcn` | Q, R | live |
| `a_short_role_name_outside_any_list_uses_roles_path_only` | S, U | live |
| `a_short_role_name_resolves_through_the_collections_list` | T | live |
| `an_inner_collections_list_replaces_the_outer_one` | AA, BA, BB, BC | live |
| `a_task_in_a_role_replaces_the_roles_meta_collections_list` | AD, AE, BD | live |
| `a_templated_entry_is_dead_but_still_replaces_the_outer_list` | DA, DB | live |
| `a_listed_collection_shadows_a_role_of_the_same_name_in_roles_path` | AF, AG | live |
| `a_plays_collections_list_reaches_a_file_it_includes` | CA, CB | ignored |

The live rows are not decoration: V, C/F/O, H/J and S/U are the over-application guards —
they pass today only because we read no list at all, and they are what stops the fix from
resolving an FQCN through the list, shadowing `ansible.legacy` unconditionally, or leaking
a play's list into a role.

## Done when

- [x] a short module name resolves through an in-scope `collections:` list — entries in
      order, ahead of `ansible.legacy`, carried by a play, a block, a task and a role's own
      `meta/main.yml`, and never leaking from a play into a role it calls (rows B, K, E, M,
      N, I, J), with an inner list *replacing* the outer rather than extending it (BA–BD)
- [x] `ns.coll.role` resolves from `collections_path` — it already did;
      `a_collection_hosted_role_resolves_by_fqcn` proves it, and `role_dir` is the site
- [x] a short *role* name resolves through the list too (row T), and beats `roles_path`
      when both hold that name (AF/AG)
- [ ] a play's list reaches a file it `include_tasks`/`import_tasks` (rows CA/CB) — needs
      the caller, not the file; ignored test in place, same dependency as [[T-068]]
- [x] a bare builtin name (`debug:`) resolves and hovers like its FQCN — bare names now
      extract and resolve in the loader's order (workspace `library/` shadowing pinned by
      `demo/library/ping.py`; order documented on `resolve_module_bare`), including the
      split-table redirect from `ansible_builtin_runtime.yml`. Corpus: module resolutions
      3665 → 5894, still 0 missing. Redirect chains and per-collection tables stay T-064.
- [x] a 2-part name (`builtin.debug`) gets an ERROR quoting the message its shape actually
      produces — `couldn't resolve module/action '…'` for the module-as-key form (the play
      never starts), `Cannot resolve … to an action or module.` for `action:`/`local_action:`
      (that task fails after earlier ones ran). Own rule id `invalid-module-name`, pinned by
      fixture in `resolve.rs` and `main.rs`

Docs: https://docs.ansible.com/ansible/latest/collections_guide/collections_using_playbooks.html
