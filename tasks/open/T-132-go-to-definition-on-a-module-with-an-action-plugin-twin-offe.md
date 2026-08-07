# T-132 — Go-to-definition on a module with an action-plugin twin offers only the module

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P3       | S    | T-124 | —          |

## Problem

Ctrl+Click on demo.charlie.beacon jumps to plugins/modules/beacon.py only. When a same-name action plugin exists, the logic lives there; today the hover's link is the only route to it.

## Approach

In `goto_definition` (`crates/ansible-lsp/src/main.rs:1791`), after resolving a module
reference, run the same twin lookup `module_hover` already does (`legacy_action_twin` /
`plugin_twin`, T-086) and append the twin to the returned `Location` array — the response
is already an array, and the client already renders multi-target references ("2 possible
targets"). Action plugin first, module second, matching the hover's "the file that runs
comes first" order.

Gate it behind a client setting (e.g. `ansibleLsp.definitionIncludesActionPlugin`,
default on) so the strict "definition = what the loader resolves" behavior stays one
toggle away. Wire it through `Settings` the same way the hint toggles flow — and note
T-125: a parsed-but-unread setting is the exact failure to avoid, so the test must cover
the off state, not just the default.

## Done when

- [ ] Ctrl+Click on a module with a same-name twin offers both files, action plugin first
- [ ] the setting turns it off, restoring module-only definitions, covered by a test
- [ ] a module with no twin still returns a single location (no picker regression)
