# T-009 — `import_playbook`

| Status | Priority | Size | Commits          |
| ------ | -------- | ---- | ---------------- |
| done   | P2       | M    | a2cc995, d6a2e85 |

## Problem

73 references, all previously dead. It's play-level rather than task-level, so it resolves
differently: importing file's directory, then the project root. No `roles_path`, no
collections.

## Outcome

`ReferenceKind::ImportPlaybook`. All 73 resolve.

Two semantics worth keeping straight, both captured in the ticket rather than the code:

- **Templated paths warn here, and only here.** Everywhere else `{{ }}` means "runtime
  unknown, stay quiet." A static import is expanded *before variables exist*, so
  `import_playbook: "{{ env }}-setup.yml"` can never resolve — the line is simply wrong. It
  returns `Missing` with an empty candidate list and its own rule id, `templated-import`,
  without touching disk. The message says why instead of listing paths that were never tried.
- **`when:` on an import is not a gate.** Ansible copies the condition onto *every task in
  every play* of the imported playbook. Captured but unused. Any future tree must label it
  *pushed down to descendants* — labelling it "conditional" would repeat the framing mistake
  that killed T-011.

A related rule was considered and **dropped on evidence**: flagging risky conditions on
`import_playbook`. All 69 conditions in the repo already use `| default(...)` or
`is defined`, so the rule would have been a pure false-positive generator.
