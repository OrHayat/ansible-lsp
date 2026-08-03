# T-075 — The startup scan blocks all requests

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | M    | —          |

## Problem

Until the workspace scan finishes, ansible-lsp is dead: no go-to-definition, no hover, and the
reference/variable colouring never paints (the file reads as plain bland YAML). On a 56-file
demo that's ~5.7 s of an unusable editor; on a large repo or a weak machine (M3 Pro) it's worse.

The cause is not the scan's cost (that's T-076) — it's that the scan runs **on the message
pump**. `initialized` awaits `scan_workspace` inline (`main.rs:1193`), and tower-lsp services
the `initialized` notification to completion before it pulls the next message. So every
`hover` / `definition` / `ansible/references` request the client fires on open sits queued
behind the whole scan. Measured from the metrics work (T-074) and confirmed by observation:
features come alive only when the scan line prints.

This is independent of speed. Even if T-076 makes the scan 10× faster, a 0.5 s freeze of every
feature on every window open is still wrong. The scan must not hold the pump.

## Options

**A — Background the scan (recommended).** Spawn `scan_workspace` as a detached task so
`initialized` returns immediately; diagnostics publish as they land (the loop already
publishes progressively). Requests are served on other tokio workers while it runs.

- Needs the state the scan touches (`client`, `docs`, `roots`, `flagged`, `mutations`) to be
  movable into a `'static` task. `client` is `Clone`; the rest are `Mutex<…>` fields on
  `Backend`. Cleanest is to group them behind an `Arc` (e.g. `Arc<SharedState>`) shared by the
  `Backend` and the spawned task, rather than making the handler own `self`.
- The scan does synchronous CPU/IO in an async fn; pair the spawn with `spawn_blocking` (or a
  blocking chunk loop) so it doesn't monopolise an async worker either.
- Pro: fixes the symptom fully, small conceptual change. Con: a modest field-ownership
  refactor; must confirm an edit arriving mid-scan (didChange invalidation) still behaves.

**B — Analyse open/visible files first, then background the rest.** Same as A, but before
spawning the workspace pass, synchronously analyse the documents already open so the file the
user is looking at is live in milliseconds. Strictly an add-on to A.

- Pro: the active file never waits on unrelated files. Con: only meaningful once A exists;
  the client also re-requests `ansible/references` on open, so A alone may already cover it.

**C — Do nothing structural, just make the scan fast (T-076 only).** Rejected as the *sole*
fix: it shrinks the freeze but doesn't remove it, and the freeze-every-open pattern is the
complaint.

## Done when

- [ ] `initialized` returns without waiting for the workspace scan
- [ ] hover / go-to-definition / colouring work on an open file while the scan is still running
- [ ] diagnostics still publish progressively and the stale-clearing pass still runs
- [ ] an edit during the scan invalidates and republishes correctly (no lost or duplicated diags)

## Refs

Surfaced by T-074 (metrics). Pairs with T-076 (the scan's actual cost); they compound —
backgrounding stops the freeze, T-076 stops the wasted work.
