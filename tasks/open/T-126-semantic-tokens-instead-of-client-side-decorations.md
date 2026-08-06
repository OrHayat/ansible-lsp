# T-126 — Semantic tokens instead of client-side decorations

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-124 | —          |

## Problem

The teal colouring on resolvable references — the project's most visible feature — is not an
LSP feature at all. It is two `TextEditorDecorationType`s in `client/src/extension.js:71`,
painted from the custom `ansible/references` request.

That works in VS Code and **only** in VS Code. T-026 (Neovim via `lspconfig`) is explicitly
"no new Rust", and this is the part that cannot survive the move: an lspconfig entry gets
diagnostics, definition, hover and document links for free, and gets no colour whatsoever.
So the headline affordance silently disappears on the second editor this project targets.

`textDocument/semanticTokens` is the protocol answer. Every client implements it, and the
token types map onto what we already know per reference.

## Approach

The data already exists — `resolved_references` (`main.rs:771`) returns exactly the spans
that get painted today. This is a second consumer of the same computation, not a new
analysis.

Design decisions to settle first, since they are visible and hard to change later:

- which standard token types to use, and whether resolved-vs-missing is a token **modifier**
  rather than a type
- range vs full-document requests (full is fine at this file size, and simpler)
- whether the custom `ansible/references` request stays for the VS Code decorations or is
  retired in favour of tokens everywhere — retiring it is tidier but loses the exact colour
  control the demo relies on

Keeping both is the likely answer, and it is worth writing down which is authoritative.

## Done when

- [ ] `semanticTokensProvider` is advertised and served
- [ ] resolvable references are coloured in a client with no custom code
- [ ] a Neovim client shows the same colouring VS Code does, verified not assumed
- [ ] the relationship between tokens and `ansible/references` is documented in the README
