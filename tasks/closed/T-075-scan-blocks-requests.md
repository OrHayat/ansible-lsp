# T-075 — The startup scan blocks all requests

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| done   | P2       | M    | —          |

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

- [x] `initialized` returns without waiting for the workspace scan
- [x] hover / go-to-definition / colouring work on an open file while the scan is still running
- [x] diagnostics still publish progressively and the stale-clearing pass still runs
- [x] an edit during the scan invalidates and republishes correctly (no lost or duplicated diags)
      — guarded in code (publish-time open-buffer re-check, `flagged` merge, VarCache epoch)
      and tests are green; not exercised by hand, accepted as closed on the code guards
- [x] the WSL machine no longer freezes for the length of the scan

## What shipped

Option A. `Backend`'s shared fields moved behind `Arc<State>` (per-field `Mutex`es unchanged);
`initialized` spawns `scan_workspace(state, client)` detached, per-file analysis under
`spawn_blocking`. New races from edits interleaving with the scan are closed by: an
open-buffer re-check at publish time (not just read time), merging `still_flagged` instead of
overwriting, never clearing an open buffer's diagnostics from the scan, and an epoch counter
on the var cache so an entry computed from pre-edit disk content is discarded rather than
inserted after an invalidation.

## Measurements — before vs after this fix

Apple M3 Pro (12-core, 36 GB RAM), macOS 26.6, release build, demo workspace (57 files
analysed of 60), warm caches. `node scripts/bench-t075.js <binary> demo/` measures from the
`initialized` notification: first `documentLink` / `hover` response on `demo/tasks/main.yml`,
and the arrival of the T-074 scan log line. Three runs each:

|                  | documentLink | hover                     | scan line |
| ---------------- | ------------ | ------------------------- | --------- |
| before (b73318d) | ~11 ms       | ~38 ms — equals scan end  | 33–49 ms  |
| after            | ~7 ms        | ~7 ms                     | 33–41 ms  |

The before-signature is the bug: hover latency exactly tracks scan completion (queued behind
the pump). After, requests answer mid-scan and the scan itself costs the same. Two findings
from measuring:

- The demo scan is only ~40 ms on this Mac; T-074's 5.7 s reference is from WSL, where
  per-file IO is ~100× slower — same T-076 redundancy, far bigger multiplier. Wall clock
  here can't validate T-076; the walked-vs-unique-files counter can.
- ~~The freeze *felt* on this Mac was mostly cold `ansible --version`~~ — **wrong, corrected
  by T-084.** The 3.6 s cold / 0.35 s warm was measured on the bare command, never through
  `detect()`, which takes a filesystem fast path that execs nothing (1 ms measured on WSL).
  The 3.6 s itself is macOS's first-exec security assessment, not Python startup. Detect did
  hold the pump and T-084 moved it off, but it was not the cost it looked like.

## WSL (the machine the complaint came from)

Same demo workspace, from the editor's LSP log rather than the bench:

```
ready — initializationOptions: ...   13:49:45
scan: 57 files analysed of 60 seen in 6113 ms (parse 2, context 355, var-index 5183, resolve 244)   13:49:51
```

6113 ms confirms T-074's ~5.7 s reference — ~150× the Mac's ~40 ms for identical work, and the
session stayed usable across those six seconds, which is the whole point of the ticket.
Recorded as observed behaviour, not as a bench table: `bench-t075.js`'s latency columns were
not re-run under WSL, where the pre-fix signature (hover landing at ~6100 ms) would have been
far more legible than the Mac's 38 vs 7.

**Var-index is 5183 of 6113 ms — 85% of the scan, against 2 ms of parsing.** This is the
T-076 evidence the Mac couldn't produce (see the note above about wall clock being useless at
40 ms). Backgrounding removed the freeze; the five seconds of largely redundant IO are still
there to delete.

## Refs

Surfaced by T-074 (metrics). Pairs with T-076 (the scan's actual cost); they compound —
backgrounding stops the freeze, T-076 stops the wasted work. Startup detect cost: T-084.
