# T-045 — Keyword schema from Ansible's FieldAttributes

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| done   | P1       | M    | —          |

## Problem

To tell a task's *module* from its *directives* (`when`/`loop`/`register`/… vs `command`),
the AST needs the authoritative set of Ansible keywords. Hand-maintaining that list rots.

## Approach

Snapshot the keyword sets from Ansible's own `FieldAttribute` declarations
(`lib/ansible/playbook/{base,play,task,block}.py` plus the `Conditional`/`Taggable`/
`CollectionSearch`/`Delegatable`/`Notifiable` mixins) into `crate::keywords`. Checked-in so
the LSP works with no Ansible clone; the module doc records how to regenerate. Lists err
toward completeness — a missed directive would be misread as a module, an extra one is
harmless.

## Done when

- [x] play/task/block directive sets derived from Ansible's definitions
- [x] `is_task_directive` / `is_play_directive` / `is_block_directive` / `is_play`
- [x] tests pin `when`/`loop`/`register`/`with_*`/`async` as directives, modules excluded

Resolution: commit `96c724b`.
