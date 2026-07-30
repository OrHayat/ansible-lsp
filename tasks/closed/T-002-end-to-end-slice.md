# T-002 — End-to-end slice: `include_tasks` + VS Code client

| Status | Priority | Size | Commit  |
| ------ | -------- | ---- | ------- |
| done   | P1       | M    | d722abb |

## Problem

Prove the whole pipe — cargo build, client spawn, LSP handshake, position encoding — while
there was exactly one moving part to debug.

## Outcome

Workspace with `ansible-core` (no LSP, no bindings) and `ansible-lsp` (tower-lsp shim), plus
a ~50-line plain-JS client with no build step. `textDocument/definition` on
`include_tasks`/`import_tasks`, literal values only, inline and block `file:` forms.

Path resolution follows Ansible's `path_dwim_relative`: role `tasks/` -> role dir -> file
dir -> project root, first hit wins.

The four real references that resolve against the role's `tasks/` dir rather than their own
directory are pinned as regression tests — a naive check ships with 4 false positives:

| Site                                                   | Target                                            |
| ------------------------------------------------------ | ------------------------------------------------- |
| `roles/lustre-snapshot/tasks/query/timestamp.yml:21`     | `query/exists.yml`                                |
| `roles/ad/tasks/join.yml:28`                           | `../../playbooks/tasks/select-available-node.yml` |
| `roles/dashboard-docker/tasks/sanity-tests/main.yml:23` | `sanity-tests/database-tests.yml`                 |
| `roles/dashboard-docker/tasks/sanity-tests/main.yml:33` | `sanity-tests/celery-tests.yml`                   |
