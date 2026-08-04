# Board

Mini issue tracker. One markdown file per ticket. **Status is the folder** — closing a
ticket is `git mv tasks/open/T-0NN-*.md tasks/closed/`, so `ls tasks/open` is always the
real backlog and `git log` records when each one moved.

```
tasks/open/     not done
tasks/closed/   done, or decided against (status: rejected inside the file)
```

Conventions:

| Field    | Values                                                                     |
| -------- | -------------------------------------------------------------------------- |
| Priority | **P1** the tool lies or goes silent · **P2** coverage/usability · **P3** nice |
| Size     | **S** <½ day · **M** ~1 day · **L** multi-day                              |
| Status   | `open` · `done` · `rejected`                                               |

IDs are never reused, including by rejected tickets — T-011 stays burned so the reasoning
in it stays findable.

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
| T-033 | [`when:` vars defined nowhere](open/T-033-undefined-when-vars.md) | L    | —          |
| T-037 | [Vault awareness](open/T-037-vault-awareness.md)             | M    | —          |
| T-067 | [Role search order doesn't match Ansible's](open/T-067-role-search-order.md) | M | — |
| T-068 | [`role_path` from invocation chains](open/T-068-chain-derived-role-path.md) | L | T-020 |

### P2 — coverage and usability

| ID    | Title                                                    | Size | Refs |
| ----- | -------------------------------------------------------- | ---- | ---- |
| T-015 | [`src:` + local-vs-remote table](open/T-015-src-paths.md) | L    | 385  |
| T-019 | [Package as a .vsix](open/T-019-package-vsix.md)          | M    | —    |
| T-020 | [Reverse index](open/T-020-reverse-index.md)              | M    | —    |
| T-016 | [`vars_files`](open/T-016-vars-files.md)                  | M    | 75   |
| T-017 | [`include_vars`](open/T-017-include-vars.md)              | M    | 30   |
| T-018 | [`meta` dependencies](open/T-018-meta-dependencies.md)    | S    | 3    |
| T-034 | [Templating that only looks dynamic](open/T-034-statically-knowable-templating.md) | M | 21 |
| T-031 | [`import_playbook` + `when:`](open/T-031-import-playbook-when.md) | M | 48 |
| T-032 | [Static `when:` evaluation](open/T-032-static-when.md)    | L    | 2669 |
| T-038 | [Resolve file-hitting lookups](open/T-038-file-lookups.md) | M   | —    |
| T-039 | [`requirements.yml` ↔ installed collections](open/T-039-requirements-collections.md) | M | — |
| T-040 | [Jinja `include`/`extends` in templates](open/T-040-jinja-template-includes.md) | L | — |
| T-041 | [`meta/argument_specs.yml` role signatures](open/T-041-role-argument-specs.md) | M | — |
| T-042 | [Resolver gaps: collections, `*_from`](open/T-042-resolver-gaps.md) | S | — |
| T-075 | [Startup scan blocks all requests](open/T-075-scan-blocks-requests.md) | M | — |
| T-076 | [Var-index re-walks shared files per consumer](open/T-076-var-index-redundant-walk.md) | M | T-075 refs |
| T-077 | [Real-repo tests read a caller's home dir](open/T-077-tests-use-fixtures-not-home.md) | M | — |

T-031 and T-032 are **partly done** — the classifier, inlay hints and four warning rules
shipped; the tree consumer and the code action didn't.

Counts are grep estimates with comments excluded — treat as ±5%. Of T-015's 385, roughly 256
are local paths worth checking and the rest are paths on the managed host.

T-018's 3 is exact, and it's the one to read before starting: an earlier count of 39 was almost
entirely unrelated matches, so it's a prerequisite for T-021's correctness rather than a
navigation win.

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
| T-070 | [`inventory_dir` from real inventory sources](open/T-070-inventory-dir.md) | M | — |
| T-080 | [Resolution-aware FQCN, exempting local modules](open/T-080-resolution-aware-fqcn.md) | M | T-042/T-064, T-025 |
| T-079 | [`Extract to collection` refactor](open/T-079-extract-legacy-plugin-to-collection.md) | L | T-020 |

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
| T-036 | [libyaml parser, matches Ansible](closed/T-036-parse-what-ansible-parses.md) | done |
| T-043 | [Docs stale after the parser swap](closed/T-043-docs-stale-after-parser-swap.md) | done |
| T-074 | [Startup scan metrics](closed/T-074-startup-scan-metrics.md)   | done     |
| T-073 | [Legacy `action_plugins/` dirs in the hover twin check](closed/T-073-legacy-action-plugin-dirs.md) | done |
| T-078 | [`when:` explanation hijacks the module-name hover](closed/T-078-when-hover-hijacks-module-hover.md) | done |

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
