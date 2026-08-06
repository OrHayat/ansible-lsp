# Board

Mini issue tracker. One markdown file per ticket. **Status is the folder** — `ls tasks/open`
is always the real backlog and `git log` records when each one moved. Lifecycle edits go
through the board CLI (T-081), which moves the file, flips its status line, keeps the tables
below in sync, and strikes a closed ticket out of other tickets' Blocked-by cells
(`~~T-0NN~~` = no longer blocking):

```
cargo run -p board -- new "Title" -p P1|P2|P3 -s S|M|L [-k bug|epic] [-e T-090] [-b T-020,T-062]
cargo run -p board -- close T-0NN [--rejected]
cargo run -p board -- reopen T-0NN
cargo run -p board -- sync T-0NN        # after hand-editing priority/size/kind/blocked-by
cargo run -p board -- list [-p P1] [-s S] [-k bug] [-e T-090] [--unblocked] [--closed]
cargo run -p board -- show T-0NN        # an epic also lists its children
cargo run -p board -- upstream [NAME]   # the dossiers in upstream/
```

```
tasks/open/     not done
tasks/closed/   done, or decided against (status: rejected inside the file)
```

Conventions:

| Field    | Values                                                                     |
| -------- | -------------------------------------------------------------------------- |
| Priority | **P1** the tool lies or goes silent · **P2** coverage/usability · **P3** nice |
| Size     | **S** <½ day · **M** ~1 day · **L** multi-day                              |
| Kind     | `task` (default) · `bug` we shipped it wrong · `epic` a parent for others   |
| Status   | `open` · `done` · `rejected`                                               |

IDs are never reused, including by rejected tickets — T-011 stays burned so the reasoning
in it stays findable.

**Kind** is a column in each ticket's header table, read by name — the tickets written
before it existed have no such column and count as `task`. Non-task kinds carry a badge in
the tables below (`**bug** ·`); tasks stay unbadged, so those rows never had to change.

**Epics** link both ways: the child's header table names its `Epic`, and the epic file
carries a `## Children` checklist the CLI ticks on close and unticks on reopen. The link is
*not* a blocker — a child is workable the moment it's filed, and `list --unblocked` still
shows it. Epics group work; `Depends on` gates it. Closing an epic with open children is
refused (`--rejected` drops it anyway), because a closed parent over open children is
exactly the drift the `board.rs` test exists to catch.

**Upstream findings are not tickets.** A bug in `ansible/ansible` is a prose dossier in
[`upstream/`](../upstream/), indexed by `board upstream` — never also a `tasks/` ticket, so
one finding never has two homes. What *we* do about it is an ordinary ticket that cites the
dossier.

## Where the project actually is

Whole-repo scan of `~/app/ansible` (kind breakdown from
a reference run with Ansible installed — `module` resolution needs it):

```
729 files, 0 unparseable

kind              resolved  missing  skipped
import_playbook         73        0        0
import_tasks            32        0        0
include_tasks          301        0        8
module                3665        0        1
role                   381        1       16
tasks_from              87        0        0
```

The one `missing` is a genuine break in that repo (T-030), not a resolver bug. The 16
skipped roles are all `cib-batch`, which legitimately has no `tasks/main.yml`. The last
`unparseable` (`_run.yml`) went to 0 when we swapped to a libyaml parser (T-036).

`when:` analysis (T-032) classifies 1053 of 2669 conditions and finds **zero** broken ones
— this repo is clean on the four provable-fault rules.

## Open

### P1 — the tool lies or goes silent

| ID    | Title                                                       | Size | Blocked by |
| ----- | ----------------------------------------------------------- | ---- | ---------- |
| T-012 | [File watcher + precise invalidation](open/T-012-file-watcher.md) | L | T-020   |
| T-037 | [Vault awareness](open/T-037-vault-awareness.md)             | M    | —          |
| T-051 | [Variable definedness diagnostic](open/T-051-variable-definedness.md) | M | T-048, T-049 |
| T-059 | [Call sites must satisfy the callee's required vars](open/T-059-caller-unpassed-vars.md) | L | T-051 |
| T-060 | [`suspicious-var`: guarded, undefined, one edit from a real name](open/T-060-suspicious-var-near-miss.md) | M | T-062 |
| T-062 | [Index ini inventories and extension-less `group_vars`](open/T-062-inventory-var-sources.md) | M | — |
| T-065 | [`var-undefined`: raise the bar to workspace-wide absence](open/T-065-var-undefined-workspace-bar.md) | M | T-051 |
| T-067 | [Role search order doesn't match Ansible's](open/T-067-role-search-order.md) | M | — |
| T-068 | [`role_path` from invocation chains](open/T-068-chain-derived-role-path.md) | L | T-020 |
| T-083 | [`ansible.legacy` unmodelled; `ansible.builtin` skips routing](open/T-083-legacy-builtin-routing.md) | M | refs T-042/1, T-064 |
| T-086 | [`plugin_twin` matches a POSIX substring, so it finds nothing on Windows](open/T-086-plugin-twin-windows-separators.md) | S | — |

### P2 — coverage and usability

| ID    | Title                                                    | Size | Refs |
| ----- | -------------------------------------------------------- | ---- | ---- |
| T-015 | [`src:` + local-vs-remote table](open/T-015-src-paths.md) | L    | 385  |
| T-019 | [Package as a .vsix](open/T-019-package-vsix.md)          | M    | —    |
| T-020 | [Reverse index](open/T-020-reverse-index.md)              | M    | —    |
| T-017 | [`include_vars`](open/T-017-include-vars.md)              | M    | 30   |
| T-034 | [Templating that only looks dynamic](open/T-034-statically-knowable-templating.md) | M | 21 |
| T-031 | [`import_playbook` + `when:`](open/T-031-import-playbook-when.md) | M | 48 |
| T-032 | [Static `when:` evaluation](open/T-032-static-when.md)    | L    | 2669 |
| T-038 | [Resolve file-hitting lookups](open/T-038-file-lookups.md) | M   | —    |
| T-039 | [`requirements.yml` ↔ installed collections](open/T-039-requirements-collections.md) | M | — |
| T-040 | [Jinja `include`/`extends` in templates](open/T-040-jinja-template-includes.md) | L | — |
| T-041 | [`meta/argument_specs.yml` role signatures](open/T-041-role-argument-specs.md) | M | — |
| T-042 | [Resolver gaps: collections, `*_from`](open/T-042-resolver-gaps.md) | S | — |
| T-046 | [Harden the module/args split](open/T-046-module-args-split.md) | M | T-044 |
| T-057 | [Module `DOCUMENTATION`/`RETURN` schema](open/T-057-module-doc-schema.md) | L | T-046 |
| T-061 | [`undeclared-var`: the playbook's required `-e` inputs](open/T-061-undeclared-var-contract.md) | S | T-062 |
| T-063 | [Full `include_role`/`import_role` parameter surface](open/T-063-include-role-params.md) | M | — |
| T-064 | [Plugin routing: redirects, deprecations, tombstones](open/T-064-plugin-routing.md) | M | — |
| T-076 | [Var-index re-walks shared files per consumer](open/T-076-var-index-redundant-walk.md) | M | T-075 refs |
| T-085 | [The var walk is syscall-bound, 4× repeats](open/T-085-walk-is-syscall-bound.md) | M | T-076 |
| T-077 | [Real-repo tests read a caller's home dir](open/T-077-tests-use-fixtures-not-home.md) | M | — |
| T-081 | [The board is hand-edited, and it has drifted](open/T-081-board-generated-not-hand-edited.md) | S | — |
| T-087 | [Invalid vars_files entry: provably fatal at runtime, silent in the editor](open/T-087-invalid-vars-files-entry-provably-fatal-at-runtime-silent-in.md) | S    | —    |

T-031 and T-032 are **partly done** — the classifier, inlay hints and four warning rules
shipped; the tree consumer and the code action didn't.

Counts are grep estimates with comments excluded — treat as ±5%. Of T-015's 385, roughly 256
are local paths worth checking and the rest are paths on the managed host.

T-018 landed, and its count of 3 was exact: an earlier count of 39 was almost entirely
unrelated matches. It mattered for T-021's correctness rather than as a navigation win, so
T-021 is now unblocked on that side.

### P3 — on top of the index / later

| ID    | Title                                                          | Size | Blocked by |
| ----- | -------------------------------------------------------------- | ---- | ---------- |
| T-021 | [`unused-file` / `unused-role` as faded hints](open/T-021-unused-hints.md) | M | T-020 |
| T-022 | [`circular-include` warning](open/T-022-circular-include.md)    | S    | T-020      |
| T-023 | [`shadowed-file` / `duplicate-role` hints](open/T-023-shadowed-and-duplicate.md) | M | — |
| T-024 | [Execution tree as a TreeView](open/T-024-execution-treeview.md) | L   | —          |
| T-025 | [Settings — toggle rules, override severity](open/T-025-settings.md) | M | —      |
| T-026 | [Neovim lspconfig entry](open/T-026-neovim.md)                  | S    | —          |
| T-027 | [Differential harness vs the legacy plugin](open/T-027-differential-harness.md) | M | — |
| T-028 | [`notify:` -> handler resolution](open/T-028-notify-handlers.md) | M   | —          |
| T-029 | [Hover showing the candidates tried](open/T-029-hover-candidates.md) | S | —      |
| T-035 | [Evaluate `when:` under a run profile](open/T-035-evaluate-when-under-profile.md) | L | T-033 |
| T-054 | [Find variable references](open/T-054-variable-references.md)    | M    | T-049, T-020 |
| T-058 | [Warn when a module ships no `DOCUMENTATION`](open/T-058-module-doc-missing-warn.md) | S | T-057, T-010 |
| T-070 | [`inventory_dir` from real inventory sources](open/T-070-inventory-dir.md) | M | — |
| T-071 | [`unconstrained-path-var`: the value set a path implies](open/T-071-unconstrained-path-var.md) | S | — |
| T-080 | [Resolution-aware FQCN, exempting local modules](open/T-080-resolution-aware-fqcn.md) | M | T-042/T-064, T-025 |
| T-079 | [`Extract to collection` refactor](open/T-079-extract-legacy-plugin-to-collection.md) | L | T-020 |
| T-088 | [Unknown play keyword: Ansible refuses the play, the editor says nothing](open/T-088-unknown-play-keyword-ansible-refuses-the-play-the-editor-say.md) | S    | —          |
| T-089 | [Indexed access into static list vars: no element support, out-of-bounds unflagged](open/T-089-indexed-access-into-static-list-vars-no-element-support-out.md) | M    | —          |

### Downstream — not this repo's code

| ID    | Title                                                          | Size |
| ----- | -------------------------------------------------------------- | ---- |
| T-030 | [`site.yml:6` references a role that doesn't exist](open/T-030-fix-lustre-reference.md) | S |

## Closed

| ID    | Title                                                          | Outcome  |
| ----- | -------------------------------------------------------------- | -------- |
| T-001 | [YAML crate spike](closed/T-001-yaml-crate-spike.md)           | done     |
| T-002 | [End-to-end slice: `include_tasks` + VS Code client](closed/T-002-end-to-end-slice.md) | done |
| T-003 | [`ansible.cfg` roots](closed/T-003-ansible-cfg-roots.md)       | done     |
| T-004 | [Roles: `include_role`, `roles:`, `tasks_from`](closed/T-004-role-references.md) | done |
| T-005 | [FQCN module and action-plugin navigation](closed/T-005-module-navigation.md) | done |
| T-006 | [`missing-file` diagnostics + repo-wide scan](closed/T-006-diagnostics.md) | done |
| T-007 | [Templated path globbing](closed/T-007-templated-globbing.md)  | done     |
| T-008 | [Teal decoration for resolvable references](closed/T-008-decoration.md) | done |
| T-009 | [`import_playbook`](closed/T-009-import-playbook.md)           | done     |
| T-010 | [`# noqa` suppression, rule-scoped](closed/T-010-noqa-suppression.md) | done |
| T-011 | [Execution tree via LSP call hierarchy](closed/T-011-call-hierarchy-tree.md) | **rejected** |
| T-014 | [README is stale](closed/T-014-readme-drift.md)                | done     |
| T-013 | [Unparseable files flagged as errors](closed/T-013-unparseable-hint.md) | done |
| T-018 | [`meta/main.yml` dependencies](closed/T-018-meta-dependencies.md) | done |
| T-033 | [`when:` vars defined nowhere](closed/T-033-undefined-when-vars.md) | done |
| T-036 | [libyaml parser, matches Ansible](closed/T-036-parse-what-ansible-parses.md) | done |
| T-043 | [Docs stale after the parser swap](closed/T-043-docs-stale-after-parser-swap.md) | done |
| T-044 | [Semantic AST (Play / Block / Task / Role)](closed/T-044-semantic-ast.md) | done |
| T-045 | [Keyword schema from Ansible's FieldAttributes](closed/T-045-keyword-schema.md) | done |
| T-047 | [Resolvers moved onto the AST](closed/T-047-resolvers-on-ast.md) | done |
| T-048 | [Variable definition index (in-file + cross-file)](closed/T-048-variable-index.md) | done |
| T-049 | [Variable uses with byte-accurate spans](closed/T-049-variable-uses.md) | done |
| T-050 | [Go-to-definition for variables](closed/T-050-variable-goto-definition.md) | done |
| T-052 | [Variable hover: where it's defined, and its value](closed/T-052-variable-hover.md) | done |
| T-053 | [Remaining variable-definition sources](closed/T-053-remaining-var-sources.md) | done |
| T-055 | [Cache the cross-file variable index](closed/T-055-variable-perf.md) | done |
| T-056 | [Expand known-value variables in templated paths](closed/T-056-expand-known-vars-in-paths.md) | done |
| T-066 | [Hover provenance breadcrumb](closed/T-066-hover-provenance-breadcrumb.md) | done |
| T-074 | [Startup scan metrics](closed/T-074-startup-scan-metrics.md)   | done     |
| T-073 | [Legacy `action_plugins/` dirs in the hover twin check](closed/T-073-legacy-action-plugin-dirs.md) | done |
| T-078 | [`when:` explanation hijacks the module-name hover](closed/T-078-when-hover-hijacks-module-hover.md) | done |
| T-075 | [Startup scan blocks all requests](closed/T-075-scan-blocks-requests.md) | done |
| T-084 | [Cold `ansible --version` blocks startup](closed/T-084-cold-ansible-detect.md) | done — A only, B/C rejected on measurement |
| T-082 | [Hover markdown is assembled by hand](closed/T-082-hover-markdown-built-by-hand.md) | done |
| T-072 | [Network modules: one platform action plugin per family](closed/T-072-network-platform-action-plugins.md) | done |
| T-016 | [`vars_files`](closed/T-016-vars-files.md)                     | done     |

## Settled — don't re-derive these

Each was verified against real Ansible or the real corpus, and each is pinned by a test.
Re-opening any of them needs new evidence, not an argument.

**Ambiguous paths: first match wins, silently — and the role's `tasks/` dir beats the including
file's own directory.** The opposite of what most people assume. Verified by running real
`ansible-playbook` against a constructed fixture, not by reading docs.
→ `matches_ansible_when_a_name_exists_in_two_search_paths`, `tests/fixtures/ambiguous/`

**Byte offsets, not character offsets — but LSP still wants UTF-16.** libyaml's marks are byte
offsets, so `parse_libyaml.rs` builds byte spans directly (no char->byte conversion, unlike the
old saphyr parser). The one remaining coordinate mismatch is bytes vs LSP UTF-16, which shifts
ranges on any line with non-ASCII — this repo has em dashes and emoji in names.
→ `spans_are_byte_accurate_past_non_ascii`, `emoji_span_slices_exactly`

**Match Ansible's parser, not the YAML 1.2 spec (T-036).** `roles/lustre-nvme-binding/tasks/_run.yml:45`
— a multi-line double-quoted scalar whose continuation lines aren't indented past their key —
is invalid YAML 1.2 (the old saphyr and yaml-rust2 parsers reject it) but PyYAML/libyaml accept
it, so it runs in production. We parse with `libyaml-safer`, which accepts exactly what Ansible
accepts (729/729 corpus). **Supersedes** the earlier "unparseable must mean no references, never
broken" rule: a file we can't parse is one Ansible can't load either, so it's a real error.
→ `accepts_the_underindented_scalar_class`, `parse_libyaml::corpus_smoke`

**`when:` on an `import_playbook` is not a gate.** Verified against ansible-core 2.20.4
source and live runs, not the docs:

- it is **prepended** to each task's own `when:` — an AND, so a task cannot opt out
  (`playbook_include.py:130-132`)
- it lands on `pre_tasks + roles + tasks + post_tasks`, **not handlers**. A handler already
  notified still fires after the gate flips.
- the play still runs: banner prints, hosts matched, recap shows `skipped=N`
- **the implicit fact gathering is skipped with it** — `play._included_conditional`
  (`playbook_include.py:114`) read by `play_iterator.py:172`. Added in Ansible 2.3,
  PR #21734, for issue #21528. Before 2.3 facts *were* gathered; not worth encoding.

An earlier tooltip claimed "facts are still gathered". It was wrong, and it was wrong
because it was written from reasoning instead of from a run — the same mistake the
first-match-wins and char-offset findings exist to prevent.
→ `task_conditions_attach_to_the_reference`, `demo/imported_semantics.yml`

**`vars_files` searches two places, and a miss is silent.** An entry resolves against
`<play dir>/vars/<entry>` — skipped when the entry already starts with the `vars`
component, so it never doubles into `vars/vars/` — then `<play dir>/<entry>`. No role
`vars/`, no project root, no extension guessing (`foo` never finds `foo.yml`); absolute
and `~` entries are a single candidate; `../` collapses lexically first. A nested list is
one-level first-found alternatives (deeper nesting is a runtime-fatal type error), and a
**missing file is silently ignored** by ansible-core 2.21.2 — a known upstream regression,
not a design: the raise was gated on `include_delegate_to and host`, PR #80171 (2.15)
flipped that default to False making it unreachable, and PR #83259 (2.18) deleted the dead
raise leaving the stale "we raise an error" comment. Reported as ansible/ansible#80483
(open, filed by a core maintainer; #81419 closed as its duplicate); fix PR #80505 was
approved then went stale unmerged. The play runs with the variables simply never set —
live-verified against `vars/manager.py:320-383` and `dataloader.py:345-390`. Also
live-verified: an entry that names a **directory** is fatal ("Is a directory" — even as a
first-found alternative it fails instead of falling through), and a **dict** entry
(`- dir: x`, include_vars-style) is fatal — `vars_files` has no options form, no regex, no
dir loading; all of that is `include_vars` plugin surface this keyword never calls.
→ `vars_files_candidate_order_vars_subdir_wins`, `vars_files_vars_prefixed_entry_skips_the_prepend`,
`vars_files_no_role_vars_or_project_root_fallback`, `vars_files_group_first_found_wins_and_all_missing_is_one_missing`

**A `set_fact` inside an imported playbook on the variable its import is gated on
half-executes the playbook**, and because facts are host-scoped, hosts diverge. Confirmed
by a live two-host run. **ansible-lint has no rule for this** — in 26.1.1 `import_playbook`
appears only in `fqcn.py`. So `when-import-var-mutated` is novel, not a reimplementation.
→ `mutation.rs`, `finds_the_real_lustre_case`

**A templated `import_playbook` can never resolve** — a static import expands before variables
exist. The only place `{{ }}` means "wrong" rather than "unknown".
→ `templated_import_playbook_is_reported_not_skipped`

**A 3-part dotted name is a module only as a mapping key.** `example.atlassian.net` appears in
this repo's YAML as a URL, shaped exactly like an FQCN.
→ `fqcn_module_keys_are_references_but_urls_are_not`

**`debug`/`assert`/`fail`/`set_fact` are documentation-only modules.** The docstring is in
`plugins/modules/`, the code that runs is in `plugins/action/`. Modules run on the managed host;
action plugins run on the controller. Looking only in `plugins/modules/` is why other tools
dead-end here.
→ `builtin_modules_resolve_into_the_installed_ansible`, plus the lookup order in `resolve.rs`

**A role with `tasks_from` and no `tasks/main.yml` is legal.** `roles/cib-batch` is exactly
this, and 16 working references depend on it staying quiet.
→ `role_without_main_is_fine_when_tasks_from_is_given`,
`role_without_main_and_without_tasks_from_is_missing`

**Four real references resolve against the role's `tasks/` dir, not their own directory.** A
naive existence check ships with 4 false positives on day one.
→ `real_repo_regressions`

**There is no `include_playbook` in Ansible.** Task level has `include_tasks`/`import_tasks`
and roles have `include_role`/`import_role`, but playbook level has only `import_playbook` —
no dynamic variant exists. So `when:` is the *only* conditional mechanism at that level, which
is why all 48 conditional imports here use it. Never recommend `include_playbook`; the real
alternatives are `meta: end_play`, restructuring into a task file, or `--skip-tags`.
→ T-031

**Not every `{{ }}` is a runtime unknown.** `playbook_dir` is guessable from convention;
treating it as opaque left real references dead. (`inventory_dir` no longer substitutes —
it is per-host, from inventory sources, and borrowed the playbook guesses wrongly; T-070.) The general rule: expand what the file
already states, glob what it doesn't, and never expand a `vars:`/`set_fact` literal — those
sit under 22 precedence levels, so expanding one invents certainty. `scan` prints the
variables still appearing in templated paths, so "is anything left" is one command.
**Partially superseded:** this entry used to include `role_path` as "the containing role's
directory". Live runs against ansible-core showed that's the invoker's directory, not the
file's (`vars/manager.py:478-481`) — undefined outside a role chain, a different role's dir
on cross-role includes. Expansion disabled until T-068 derives it from invocation chains;
T-067 covers the role-search-order half.
→ `expand_magic`, `role_path_expands_to_the_containing_role` (ignored pending T-068), T-034

**`| default(D)` states the value when a variable is unset, which is the only thing that makes
static `when:` analysis possible** — no variable resolution, no precedence rules. It is also
what makes a typo permanent: `skip_smaba | default(false)` is false forever and silent.
→ `condition.rs`, T-033

**Stripping wrapping parens with `trim_end_matches(')')` eats the closing paren of a trailing
`default(...)`.** It silently broke every guarded comparison and four tests caught it.
→ `strip_outer_parens`
