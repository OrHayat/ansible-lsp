# T-090 — Resolver diverges from real Ansible

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | epic | P1       | L    | —          |

## Problem

The resolver was built against the documented search order. Reading the 2.22.0.dev0 tree
against `resolve.rs` and `workspace.rs` turned up eight places where it answers confidently
and wrongly — wrong extensions, wrong base directory, wrong candidate list, a false `Missing`
on a reference that runs fine.

These are one epic because they share a root cause and a fix shape: the resolver models
*a* search order rather than `DataLoader.path_dwim_relative` and `PluginLoader`, and each
child is one place that gap surfaces. Several also share `T-096`'s playbook-dir correction,
so doing them apart means doing that correction more than once.

Each child is P1 by the board's own definition — the tool lies or goes silent — and each
carries the `file:line` in ansible-core that proves it, so none of them rests on this epic's
summary being right.

The two closed children are the foundation this epic is paying off: `T-036` made the parser
match Ansible's, `T-047` moved the resolvers onto the AST. Matching Ansible's *resolution* is
the same argument applied one layer up.

## Children

- [ ] T-067 — Role search order doesn't match Ansible's
- [x] T-036 — Parse the YAML that Ansible parses (lenient oracle, maybe a swap)
- [x] T-047 — Move the resolvers onto the AST
- [x] T-091 — with_ext misses .json and extensionless, and tasks_from flips the order
- [x] T-092 — Includes inside handlers/ resolve against tasks/
- [x] T-093 — Bare module names only try .py
- [x] T-094 — short_key treats any dotted include_tasks as an include
- [ ] T-095 — Templated import_playbook is reported missing
- [ ] T-096 — project_root stands in for the playbook dir
- [ ] T-097 — include_vars searches paths Ansible never tries
- [ ] T-098 — ansible.cfg discovery ignores env overrides and the CWD rule
- [x] T-001 — YAML crate spike
- [x] T-003 — `ansible.cfg` roots
- [x] T-004 — Roles: `include_role`, `roles:`, `tasks_from`
- [x] T-005 — FQCN module and action-plugin navigation
- [x] T-006 — `missing-file` diagnostics + repo-wide scan
- [x] T-009 — `import_playbook`
- [x] T-013 — Hint on unparseable files
- [x] T-018 — `meta/main.yml` dependencies

## Done when

- [ ] every child is closed or rejected
- [ ] the corpus scan over `~/app/ansible` is re-run and any change in the resolved/missing
      counts is explained in `tasks/README.md`
