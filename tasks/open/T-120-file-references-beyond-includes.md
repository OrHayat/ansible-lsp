# T-120 — File references beyond includes

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | epic | P2       | L    | —          |

## Problem

The resolver knows six kinds of reference, all of them ways one YAML file names another.
Every remaining way a playbook points at a file on disk is unhandled, and there are more of
them than of the handled kind — `src:` alone is **385 sites** in the corpus.

| Child | What points at a file |
| ----- | --------------------- |
| T-015 | `src:` on `template`, `copy`, `unarchive`, ... — 385 sites, and some are *remote* paths |
| T-017 | `include_vars` in all three forms |
| T-034 | templating that only looks dynamic: `{{ item }}` over a literal `loop:`, `\| default('literal')` |
| T-038 | file-hitting lookups: `file`, `template`, `ini`, `csvfile`, `first_found`, `fileglob` |
| T-040 | `{% include %}` / `{% import %}` / `{% extends %}` inside `.j2` files |
| T-070 | `inventory_dir` expanded from real inventory sources |
| T-023 | which of several matches actually wins, and which are dead |

The sequencing that makes this an epic: **T-015 gates T-034**, because 11 of the 12
literal-loop cases T-034 wants to expand are `src:`. And T-015 cannot be done naively —
`src:` is local for `template:` and remote when `remote_src: true`, so it needs a per-module
Local/Remote/DependsOn table before a single diagnostic can be trusted. Get that wrong and
we warn about paths that are supposed to live on the target host.

They also share the search-path model. Ansible resolves all of these through `_find_needle` →
`path_dwim_relative_stack` over `task.get_search_path()` (`plugins/action/__init__.py:1540-1551`),
where the subdir is chosen by **substring of the action name** — `template` → `templates/`,
`var` → `vars/`, else `files/` (`plugins/lookup/first_found.py:240-245`). One model, seven
consumers. Note `path_dwim_relative` never errors: it returns the last candidate whether or
not it exists (`dataloader.py:327-331`).

T-040 is the outlier and the reason this epic is L: it needs a **second grammar**. A `.j2`
file is Jinja, not YAML, and multi-context templates mean a name can resolve differently
depending on which task rendered it — so it offers candidates rather than one answer.

## Children

- [ ] T-015 — `template:`/`copy:` `src:` + the local-vs-remote table
- [ ] T-017 — `include_vars`
- [ ] T-023 — `shadowed-file` / `duplicate-role` hints
- [ ] T-034 — Templating that looks dynamic but isn't
- [ ] T-038 — Resolve file-hitting lookups
- [ ] T-040 — Jinja `{% include %}` / `{% import %}` / `{% extends %}` in templates
- [ ] T-070 — `inventory_dir` from real inventory sources
- [x] T-007 — Templated path globbing
- [x] T-016 — `vars_files`

## Done when

- [ ] every child is closed or rejected
- [ ] one search-path model serves all of them, cited to `_find_needle`
- [ ] no remote path is ever diagnosed as missing
