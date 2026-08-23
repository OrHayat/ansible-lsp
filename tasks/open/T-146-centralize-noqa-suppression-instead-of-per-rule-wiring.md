# T-146 — Centralize noqa suppression instead of per-rule wiring

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P3       | M    | —          |

## Problem

`Document::is_suppressed` is called from **16 sites** — every diagnostic producer filters
itself, by hand, before emitting. Each new rule copies the incantation and each picks its
own anchor (`span.start`, `r.span.start`, `u.span.start`). The failure mode is silent:
forget the call on the next rule and that rule is simply unsuppressible — no test exists
that sweeps "every emitted code responds to `# noqa: <code>`", so nothing would catch it.

(The count was written as 19 when T-107 landed. Re-counted: 16 `is_suppressed` calls
against 15 `Diagnostic` constructions.)

## Sizing: this is not the mechanical refactor it looks like

Was `S`. Re-sized to `M` after checking the sites, because "delete the per-rule calls" is
the easy half and it is not the half that decides whether the result is right.

**The wiring is already complete, so there is nothing to find by grep.** All 15 `Diagnostic`
constructions carry a `code`, and each has a matching `is_suppressed` filter directly above
it — 1:1, no gaps. So "some rules are missing suppression" is not a wiring oversight that a
sweep test would surface. It is a question about which rules *ought* to be suppressible, and
that has to be answered one rule at a time.

**And some of the existing wiring is wrong.** `unparseable` and `inventory-not-yaml`
(`main.rs:567`) are both `# noqa`-suppressible ERRORs. The first one says "Ansible's parser
rejects this too, so a play that loads this file will fail" — and offers a comment that makes
the squiggle go away while the play still fails. The suppression is also read out of the very
file that failed to parse. Silencing that is the ignore-the-squiggle training this project's
rules open with, and it should probably be removed rather than centralised. Deciding that is
a judgement per rule, not a refactor.

So the work is: audit all 15, decide suppressible-or-not and the right anchor for each,
*then* centralise what survives. Goes `L` if the audit turns up rules whose behaviour has to
change rather than just their wiring.

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

- [ ] each of the 15 emission sites has a recorded verdict — suppressible or not, and why —
      before any code moves; the audit is the deliverable that makes the rest safe
- [ ] `unparseable` / `inventory-not-yaml` settled explicitly: kept with a reason, or removed
- [ ] one suppression pass at the diagnostic choke point; per-rule `is_suppressed`
      calls deleted
- [ ] a sweep test: every diagnostic code the server can emit is silenced by
      `# noqa: <its code>` on the flagged line
- [ ] a new rule added with no suppression code at all is suppressible for free
