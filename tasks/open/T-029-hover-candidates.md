# T-029 — Hover showing the candidates tried

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P3       | S    | —          |

## Problem

`Resolution` already carries `candidates: Vec<PathBuf>` — every path tried, in order — and
nothing shows it unless the reference is missing. When resolution *succeeds* the information is
computed and thrown away.

That's the information you want in three situations:

- **which of several matches won.** Ansible resolves ambiguity silently and the order is
  unintuitive: the role's `tasks/` dir beats the including file's own directory. T-023 warns
  about this; hover explains it on demand without adding a diagnostic.
- **why a templated reference offered these candidates** and not others.
- **which collection a module came from** — in-repo, installed, or `ansible.builtin` — and
  whether it landed in `plugins/modules/` or `plugins/action/`. The
  documentation-only-module distinction is confusing enough that showing it is worth the hover.

## Approach

`textDocument/hover`. Resolved target first, then the candidates tried, in order, marking the
one that won. Skipped references show the reason (`RemotePath`, `Templated`, `UnknownModule`)
instead — which doubles as the answer to "why isn't this coloured."

Cheap: the data already exists, this is a formatter and a handler. Carries to Neovim for free,
unlike the decoration.

Keep it short — three or four lines. A hover that fills the screen gets dismissed reflexively.

## Done when

- [ ] hover on a resolved reference shows the target and the candidates tried, in order
- [ ] the winning candidate is marked
- [ ] a skipped reference shows its skip reason
- [ ] modules show which collection and whether it's `plugins/modules/` or `plugins/action/`
