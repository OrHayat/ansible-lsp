# T-146 — Centralize noqa suppression instead of per-rule wiring

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P3       | S    | —          |

## Problem

`Document::is_suppressed` is called from **19 sites** — every diagnostic producer filters
itself, by hand, before emitting (`main.rs:299,329,495,533,561,585,627,649,…`). Each new
rule copies the incantation, each picks its own anchor (`span.start`, `r.span.start`,
`u.span.start`), and T-107 just added the 19th copy. The failure mode is silent: forget
the call on the next rule and that rule is simply unsuppressible — no test exists that
sweeps "every emitted code responds to `# noqa: <code>`", so nothing would catch it.

## Approach

Suppression belongs at the one choke point where diagnostics leave, not in every
producer. All 19 sites funnel into `Diagnostic` values that carry both facts the check
needs — a range and a `code` — so a single pass at assembly/publish time
(`diagnostics_of` / `publish_diagnostics`) can drop suppressed ones:
`is_suppressed` is line-based already (own line or the line above), so a
position-taking variant needs no byte offset. Then delete the per-rule calls.

One deliberate exception to keep: the duplicate-key path prints an `{unreported}` count
*into the message* for suppressed siblings — that one consumes suppression state, not
just filtering by it, and stays where it is.

## Done when

- [ ] one suppression pass at the diagnostic choke point; per-rule `is_suppressed`
      calls deleted
- [ ] a sweep test: every diagnostic code the server can emit is silenced by
      `# noqa: <its code>` on the flagged line
- [ ] a new rule added with no suppression code at all is suppressible for free
