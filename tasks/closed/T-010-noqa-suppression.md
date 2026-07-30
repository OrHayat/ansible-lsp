# T-010 — `# noqa` suppression, rule-scoped

| Status | Priority | Size | Commits          |
| ------ | -------- | ---- | ---------------- |
| done   | P1       | S    | 5c4ad3d, 204ecf0 |

## Problem

Every diagnostic needs an escape hatch, or one wrong warning poisons trust in all of them.
The concrete case: a file genuinely named `{{ env }}-setup.yml` on disk would get a
`templated-import` warning it can't act on.

## Outcome

ansible-lint's syntax, so there's nothing new to learn:

```yaml
- include_tasks: gone.yml   # noqa                 # everything on this line
- include_tasks: gone.yml   # noqa: missing-file   # only that rule
# noqa: missing-file
- include_tasks: gone.yml                          # line above also counts
```

Read from raw source, since comments aren't in the AST.

**The bug worth remembering:** the first cut matched `# noqa` as a substring. This repo
already carries `# noqa: command-instead-of-module` on four lines for ansible-lint — so
those lines were silently disabling our checks too. Rules are now compared by exact id.
Pinned by `another_tools_noqa_does_not_silence_ours`.

Rule ids so far: `missing-file`, `templated-import`.

Per-rule config beyond noqa (project-level disable, severity overrides) is T-025.
