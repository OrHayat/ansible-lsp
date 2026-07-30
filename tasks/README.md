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

9 commits on `main`, 63 tests, ~3000 lines. Whole-repo scan of `~/matrix/ansible`:

```
731 files, 1 unparseable

kind              resolved  missing  skipped
import_playbook         73        0        0
import_tasks            32        0        0
include_tasks          301        0        8
module                3665        0        1
role                   381        1       16
tasks_from              87        0        0
```

The one `missing` is a genuine break in that repo (T-030), not a resolver bug. The 16
skipped roles are all `cib-batch`, which legitimately has no `tasks/main.yml`.

`when:` analysis (T-032) classifies 1053 of 2669 conditions and finds **zero** broken ones
— this repo is clean on the four provable-fault rules.

## Open

### P1 — the tool lies or goes silent

| ID    | Title                                                       | Size | Blocked by |
| ----- | ----------------------------------------------------------- | ---- | ---------- |
| T-013 | [Hint on unparseable files](open/T-013-unparseable-hint.md)  | S    | —          |
| T-014 | [README is stale](open/T-014-readme-drift.md)                | S    | —          |
| T-012 | [File watcher + precise invalidation](open/T-012-file-watcher.md) | L | T-020   |
| T-033 | [`when:` vars defined nowhere](open/T-033-undefined-when-vars.md) | L    | —          |

### P2 — coverage and usability

| ID    | Title                                                    | Size | Refs |
| ----- | -------------------------------------------------------- | ---- | ---- |
| T-015 | [`src:` + local-vs-remote table](open/T-015-src-paths.md) | L    | 385  |
| T-019 | [Package as a .vsix](open/T-019-package-vsix.md)          | M    | —    |
| T-020 | [Reverse index](open/T-020-reverse-index.md)              | M    | —    |
| T-016 | [`vars_files`](open/T-016-vars-files.md)                  | M    | 75   |
| T-017 | [`include_vars`](open/T-017-include-vars.md)              | M    | 30   |
| T-018 | [`meta` dependencies](open/T-018-meta-dependencies.md)    | S    | 3    |
| T-031 | [`import_playbook` + `when:`](open/T-031-import-playbook-when.md) | M | 48 |
| T-032 | [Static `when:` evaluation](open/T-032-static-when.md)    | L    | 2669 |

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

### Downstream — not this repo's code

| ID    | Title                                                          | Size |
| ----- | -------------------------------------------------------------- | ---- |
| T-030 | [`site.yml:6` references a role that doesn't exist](open/T-030-fix-daos-reference.md) | S |

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

## Settled — don't re-derive these

Each was verified against real Ansible or the real corpus, and each is pinned by a test.
Re-opening any of them needs new evidence, not an argument.

**Ambiguous paths: first match wins, silently — and the role's `tasks/` dir beats the including
file's own directory.** The opposite of what most people assume. Verified by running real
`ansible-playbook` against a constructed fixture, not by reading docs.
→ `matches_ansible_when_a_name_exists_in_two_search_paths`, `tests/fixtures/ambiguous/`

**saphyr markers are character offsets, not byte offsets.** So there are three coordinate
systems — saphyr chars, Rust bytes, LSP UTF-16 — and mixing them is invisible in ASCII and
wrong on every line with an em dash. Converted once, in `parse.rs`.
→ `spans_are_byte_accurate_past_non_ascii`, `emoji_and_utf16_roundtrip`

**Strict YAML 1.2 is stricter than Ansible.** `roles/daos-nvme-binding/tasks/_run.yml:45` fails
in both saphyr and yaml-rust2 but PyYAML accepts it, so it runs in production. Unparseable must
therefore mean *no references*, never *broken*. "Does it parse" is not a proxy for "is it valid
Ansible."
→ `unparseable_yields_none_not_panic`

**`when:` on an `import_playbook` is not a gate.** Ansible copies it onto every task in every
imported play. Any tree or diagnostic must say *pushed down*, not *conditional*.
→ `task_conditions_attach_to_the_reference`

**A templated `import_playbook` can never resolve** — a static import expands before variables
exist. The only place `{{ }}` means "wrong" rather than "unknown".
→ `templated_import_playbook_is_reported_not_skipped`

**A 3-part dotted name is a module only as a mapping key.** `volumez.atlassian.net` appears in
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

**`| default(D)` states the value when a variable is unset, which is the only thing that makes
static `when:` analysis possible** — no variable resolution, no precedence rules. It is also
what makes a typo permanent: `skip_smaba | default(false)` is false forever and silent.
→ `condition.rs`, T-033

**Stripping wrapping parens with `trim_end_matches(')')` eats the closing paren of a trailing
`default(...)`.** It silently broke every guarded comparison and four tests caught it.
→ `strip_outer_parens`
