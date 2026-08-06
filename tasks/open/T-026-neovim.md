# T-026 — Neovim lspconfig entry

| Status | Priority | Size | Target     | Epic  |
| ------ | -------- | ---- | ---------- | ----- |
| open   | P3       | S    | ~Sept 2026 | T-130 |

## Problem

Neovim support in roughly a month was the requirement that decided the whole architecture.
This ticket is where that bet gets collected.

It's also the bet's audit: if this turns out to be more than a config file plus a smoke test,
the LSP-over-napi-rs choice was worse than it looked. Recording that either way is the point.

## Approach

An `lspconfig` entry pointing at the same binary. No new Rust — that's the claim being tested.

```lua
require('lspconfig').ansible_lsp.setup {
  cmd = { '/path/to/ansible-lsp' },
  filetypes = { 'yaml', 'yaml.ansible' },
  root_dir = require('lspconfig.util').root_pattern('ansible.cfg', '.git'),
}
```

What needs verifying rather than assuming:

- `textDocument/definition` and diagnostics — should be free
- **position encoding.** Neovim negotiates `utf-8` where VS Code uses `utf-16`. The core is
  byte-based and converts only at the LSP boundary, so this should be a one-line branch — but
  it is the single most likely thing to be silently wrong, and it'll only show up on lines with
  non-ASCII. Test with the em-dash and emoji fixtures.
- **the teal decoration does not carry.** It's a VS Code text-decoration API, not LSP. The
  `ansible/references` request works anywhere, but rendering needs an nvim-side extmark
  handler, or the "you can see what's clickable" affordance is VS Code-only.
- T-024's TreeView doesn't carry either — noted in that ticket.

## Done when

- [ ] go-to-definition and diagnostics work in Neovim against `~/app/ansible`
- [ ] ranges are correct on lines containing em dashes and emoji
- [ ] whether decoration was ported, or deliberately skipped, is recorded here
- [ ] the architecture bet is assessed in one honest line: config file, or more?
