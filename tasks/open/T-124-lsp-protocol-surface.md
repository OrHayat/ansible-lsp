# T-124 — LSP protocol surface

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | epic | P2       | L    | —          |

## Problem

What this server advertises today (`crates/ansible-lsp/src/main.rs:1671-1681`) is:
text sync, definition, hover, document links, push diagnostics, and one custom request
`ansible/references`.

Not implemented at all: **completion**, **semantic tokens**, references, code actions, rename,
formatting, document symbols, workspace symbols, code lens, folding, signature help. Call
hierarchy was implemented and rejected (T-011). `didChangeWatchedFiles` is T-012.

Most of those absences are correct — this is a resolver, not an IDE. Three are not, and they
are the children here:

- **T-125** is a bug: a setting exists for a feature that does not.
- **T-126** matters because the teal colouring is *client-side decorations*, not a protocol
  feature. T-026 (Neovim, explicitly "no new Rust") gets diagnostics and hover for free and
  gets no colour at all. The headline affordance silently disappears on the second editor
  this project targets.
- **T-127** is wanted by four tickets — T-041, T-057, T-107, T-115 — each of which would
  otherwise grow its own. It is also the *safer* first target for the name indexes: an
  incomplete index is a missing suggestion in completion and a false error in a diagnostic.

The epic exists to keep those three from being done as afterthoughts inside feature tickets,
which is how a server ends up with three ad-hoc position-detection implementations.

Explicitly **not** here: rename, formatting, symbols. No ticket asks for them, Ansible YAML
has no symbol structure worth exposing, and T-011 already recorded what happens when a
protocol feature is adopted for data that does not fit its model.

## Children

- [ ] T-125 — Inlay hints: the setting is parsed but gates nothing
- [ ] T-126 — Semantic tokens instead of client-side decorations
- [ ] T-127 — A completion provider
- [x] T-008 — Teal decoration for resolvable references
- [x] T-082 — Hover markdown is assembled by hand, in eight places
- [ ] T-132 — Go-to-definition on a module with an action-plugin twin offers only the module
- [x] T-143 — Hover is silent on magic variables, including the two whose value we detect

## Done when

- [ ] every child is closed or rejected
- [ ] the README's capability list matches `ServerCapabilities`, checked rather than asserted
- [ ] no setting exists for a feature that does not
