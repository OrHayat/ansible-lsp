# T-132 — Go-to-definition on a module with an action-plugin twin offers only the module

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P3       | S    | T-124 | —          |

## Problem

Ctrl+Click on `demo.charlie.beacon` jumps to `plugins/modules/beacon.py` only. When a
same-name action plugin exists, that is the file Ansible dispatches the task to (T-073,
T-086); today the hover's link is the only route to it.

It is not a demo-collection edge. Measured 2026-09-16 on the installed 2.21.3: **27 of 74**
builtin modules have a same-name `plugins/action/` twin, and in **19** of them the module file
is documentation only — no `AnsibleModule(` and no `def main` — among them `debug`,
`set_fact`, `template`, `shell`, `fetch` and `include_vars`. Ctrl+Click on `debug:` opens a
file with no code in it.

## Approach

**Superseded 2026-09-16:** the first plan appended the twin to the go-to-definition array
behind an on-by-default setting. Rejected, for two reasons read from VS Code's source
(`gotoSymbol/browser/referencesModel.ts`, `goToCommands.ts`) and not yet run in the editor:

- `editor.gotoLocation.multipleDefinitions` defaults to `peek`, so every Ctrl+Click on those 27
  builtins would stop jumping and open a picker instead.
- In `peek` mode `ReferencesModel` re-sorts results by URI and pre-selects by longest common
  URI prefix, so the "action plugin first" order is not ours to set. For the core layout
  `ansible/modules/` sorts ahead of `ansible/plugins/action/` and the module is pre-selected.
  Only the `goto`/`gotoAndPeek` modes honour provider order, via `firstReference()`.

**Instead: answer `textDocument/implementation`.** Definition keeps meaning "what the loader
resolves" and is untouched. Go to Implementation — `Cmd+F12` / `Ctrl+F12`, and in the editor
context menu (`goToCommands.ts`) — opens the file the task is dispatched to:

1. a same-name action plugin, in the order `module_hover` already searches it:
   `legacy_action_twin`, then `plugin_twin` (collection and core layouts)
2. else the network platform plugin (`network_platform_twin`, T-072)
3. else the module itself — with no twin, the module is what runs

That is exactly the run-order `module_hover` computes, so the lookup moves out of
`module_hover` into one helper both read (working rule 3). The hover output must not change.

One file per answer, so the request never opens a picker. What an action plugin calls in turn
(`_execute_module` on some other module) is not inferable from the task and is out of scope.

Non-module references (include paths, roles) answer nothing — definition already covers them.

No setting: the user picks the file by picking the command. `tower-lsp` 0.20 has
`goto_implementation`; the capability is `implementation_provider`. Neovim has
`vim.lsp.buf.implementation()`, so T-026 inherits it.

## Done when

- [ ] Go to Implementation on a module with a same-name twin returns the action plugin only:
      collection (`demo.charlie.beacon`), core layout, and a cfg/role `action_plugins/` dir,
      one assertion each
- [ ] a network module with no twin returns its platform plugin
- [ ] a module with neither returns the module itself
- [ ] go-to-definition is unchanged — still the module only, asserted on the twin fixture
- [ ] hover and implementation read one helper; the existing hover twin/platform tests pass
      unchanged
- [ ] `implementation_provider` is declared in `ServerCapabilities`
- [ ] a non-module reference returns nothing
- [ ] run once in VS Code: `Cmd+F12` on `debug:` opens `plugins/action/debug.py` — the editor
      behaviour above is read from source, so record the result here
