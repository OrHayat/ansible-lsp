# T-004 — Roles: `include_role`, `roles:`, `tasks_from`

| Status | Priority | Size | Commit  |
| ------ | -------- | ---- | ------- |
| done   | P1       | M    | d722abb |

## Problem

326 role references in the real repo, in every spelling Ansible allows. The legacy plugin
misses the flow form entirely and mis-binds `tasks_from` in long task blocks.

## Outcome

`ReferenceKind::Role` and `::TasksFrom`, covering:

- `include_role:` / `import_role:` with `name:`
- `roles:` list entries — bare string (`- postgres_setup`) **and** dict (`- role: podman`)
- `tasks_from:` -> that role's `tasks/<value>[.yml|.yaml]`
- the flow form `include_role: { name: cib-batch, tasks_from: begin }`

The flow form works because `tasks_from`'s owning `name:` comes from `Node::get()` on the
**same mapping node**. The legacy plugin scans ±6 lines for a sibling `name:`, which on one
line finds nothing and in a long block silently binds to the *previous* task's role.

Two things that would otherwise have been false positives:

- **`has_tasks_from`** — a role with no `tasks/main.yml` is `Skipped`, not missing, when the
  caller supplies `tasks_from`. `roles/cib-batch` is exactly this (begin/commit/abort.yml,
  no main.yml) and 16 working references depend on it staying quiet.
- `collections/*/roles/` is empty in this repo, so FQCN matters for modules, not roles. The
  lookup supports it anyway — same code path.
