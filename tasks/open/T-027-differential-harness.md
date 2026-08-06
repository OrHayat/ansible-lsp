# T-027 — Differential harness vs the legacy plugin

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P3       | M    | T-130 | —          |

## Problem

`community-local.ansible-role-goto` is disabled in favour of this server, so any reference the
old plugin resolved and this one doesn't is a **regression the user hits and I don't**. Unit
tests can't catch that class — they only cover cases someone thought to write.

The claim being made is "strict superset." Nothing currently verifies it.

## Approach

The prototype's `resolveLine()` is nearly a pure function
(`~/.vscode/extensions/community-local.ansible-role-goto-0.0.1/extension.js`, ~300 lines of line
regex). Stub the small `vscode` surface it touches (~40 lines), run both resolvers over all 731
files, and diff.

The assertion is deliberately one-directional:

- **anything the old plugin resolved that this doesn't -> blocker.** No exceptions; that's a
  user-visible regression.
- **anything only the new one resolves -> the point.** Spot-check, don't diff.
- **same reference, different target -> investigate.** Most likely correct — the old plugin
  resolves relative to the including file, which is wrong in the four cases pinned in T-002 —
  but "our answer differs and we assumed we were right" is exactly how a real regression hides.

Needs node on PATH: `export PATH=~/.nvm/versions/node/v24.11.0/bin:$PATH` (installed via nvm,
not exported in non-interactive shells).

Do this before T-019 makes the new server the only thing installed, or the safety net arrives
after the fall.

## Done when

- [ ] both resolvers run over all 731 files and produce comparable output
- [ ] zero references resolved by the old plugin and not by this one
- [ ] every differing target is explained, in this ticket
- [ ] the harness is re-runnable, so it guards future reference kinds too
