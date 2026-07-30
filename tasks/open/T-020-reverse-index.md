# T-020 — Reverse index

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | M    | —          |

## Problem

Everything the resolver does today is forward-only: *this reference points there.* Four
separate wants all need the inverse — *what points at this file?*

- **"Who includes this?"** — open a task file and you currently cannot tell what reaches it,
  which is the question you ask before editing anything shared
- **T-021** unused-file / unused-role: a file with no inbound edges
- **T-022** circular includes: a cycle in the graph
- **T-012** file watcher: republish exactly the files affected by a change, instead of
  rescanning everything

Building it once, properly, is cheaper than four partial versions.

## Approach

The scan already resolves every reference in the workspace, so this is mostly bookkeeping:
invert the results into `target path -> Vec<(source path, span, ReferenceKind)>`.

Design points worth settling before writing it:

- **Templated references produce edges to *every* candidate.** Overcounting is the safe
  direction: it can only make a file look used when it might not be, and a false "unused"
  hint is much worse than a missed one. This is precisely the case that broke a first
  grep-based estimate — `roles/access-point/tasks/protocol-base/validate-expose.yml` looked
  unreferenced but is reached via `validate-{{ _ap_op_type }}.yml`.
- **Unparseable files contribute no edges**, so anything only referenced from one looks
  unused. T-013's hint is what makes that visible instead of silent; T-021 should also
  suppress unused-hints entirely while any file fails to parse, or state the caveat.
- Keys are resolved absolute paths, so two spellings of the same target collapse.
- Exposed as a custom request (`ansible/whoReferences`) plus a command, not
  `textDocument/references` — that request is symbol-scoped, and T-011 is the record of what
  happens when file-scoped data is forced into a symbol-scoped protocol.

## Done when

- [ ] a command on any task file lists every reference reaching it, with line numbers
- [ ] templated references contribute an edge per candidate
- [ ] role dependencies (T-018) count as edges
- [ ] `scan` can print the index, so it's inspectable without an editor
- [ ] building it adds no measurable time to the existing scan
