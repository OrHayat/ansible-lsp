# T-234 — Find All References on a module name lists every task that uses it

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P3       | M    | T-124 | T-132      |

## Problem

There is no way to ask "which tasks in this workspace use `copy`?". `Shift+F12` (Find All
References, `goToCommands.ts`) on a module name answers nothing — the server declares no
`references_provider`. `ansible/whoReferences` (T-020) answers a neighbouring question, "what
reaches this *file*", and only from inside the module's own file.

Split out of T-132's discussion, which covers the other two navigation requests on a module
name: definition (the module) and implementation (what the task is dispatched to).

## Approach

T-020's note that `textDocument/references` is symbol-scoped, and T-011's rejection, are about
*file*-scoped questions. A module name under the cursor is a symbol, so this one fits the
protocol and does not repeat T-011.

The data is already indexed. Measured 2026-09-16: `scan demo --reverse` lists module files as
reverse-index targets with the tasks that reach them, e.g.
`demo/charlie/plugins/modules/beacon.py` ← `tasks/action_plugins.yml:33 module`. That CLI
passes no install, so only collection modules resolve there; the editor's scan builds its
cache `with_install(state.install())`, so builtins are expected in the editor's index too —
**not yet measured**, and the first thing to check.

So the handler is: resolve the module reference under the cursor, then
`inbound_refs(winner)`. Keying by the resolved file rather than the written name is the point —
`copy:`, `ansible.builtin.copy:` and `ansible.legacy.copy:` should give one answer if they
reach one file, and a redirect should land with its target.

Open questions, to settle before writing code:

- **A partial answer during the scan.** `whoReferences` carries a `scanning` flag and the
  client says "still running". `textDocument/references` has no such field, so an empty or
  short list mid-scan reads as "nothing else uses this". Options: log it, send a
  `window/showMessage`, or answer `None` until the scan finishes. Pick one and test it.
- **`includeDeclaration`.** The declaration is the module file itself, not a location in a
  task. Decide whether it is added when the client asks for it.
- **Roles** have the same shape (`roles:` entries and `include_role:` reaching one
  `tasks/main.yml`). In scope only if it falls out of the same handler; otherwise its own ticket.

Blocked by T-132 for sequencing, not a technical dependency: implementation lands first.

## Done when

- [ ] builtin modules are confirmed present in the editor's reverse index (measured, result
      recorded here) — or the gap is fixed first
- [ ] Find All References on a module name returns every task in the workspace that resolves
      to the same file, from a fixture with at least two users in two files
- [ ] the short name and the FQCN of one module return the same list, asserted
- [ ] a module used nowhere else returns only the position asked about, or nothing — whichever
      `includeDeclaration` decides, stated here
- [ ] the mid-scan behaviour is decided, recorded here, and tested
- [ ] `references_provider` is declared in `ServerCapabilities`
