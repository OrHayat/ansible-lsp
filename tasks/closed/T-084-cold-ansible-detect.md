# T-084 — Cold `ansible --version` blocks startup for seconds

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| done   | P2       | S    | T-131 | —          |

**Outcome: A shipped, B and C rejected on measurement.** Detect is off the pump and costs
1 ms, because the subprocess it was scoped to avoid is never reached on a working install.
The instrumentation cancelled most of its own ticket — see *What the numbers said*.

## Problem

`initialized` awaits `AnsibleInstall::detect()` inline before spawning the workspace scan
(`main.rs`), and detect's slow path shells out to `ansible --version`. Measured on the
M3 Pro (macOS 26.6, Homebrew ansible): **3.6 s cold, 0.35 s warm** — the Python interpreter
plus the full `ansible.cli` import, paid on the first open after boot or cache eviction.
While it runs, the message pump is held, so the editor is dead exactly the way T-075 fixed
for the scan. The T-074 scan log line never counts it, which is why a "53 ms" scan could
still feel like seconds — this was the actual felt freeze on the Mac.

Unverified (no time to bench yet): which of `detect()`'s paths this machine actually hits —
`install.rs` has filesystem fast paths before the subprocess fallback, and the 3.6 s was
measured on the bare `ansible --version` command, not through `detect()`.

**That caveat turned out to be the whole ticket** — see below.

## Options (not exclusive)

**A — Move detect off the pump (cheap, do regardless).** Fold detect + the "Ansible not
found" status notification into the T-075 detached task, detect first, then scan (the
scan's module resolution wants the install anyway). `initialized` returns in microseconds;
the toast arrives seconds later, which is fine. ~10 lines on top of T-075's `Arc<State>`.
Caveat: a module hover/documentLink arriving before detect finishes will block on the
`OnceLock` — rare, one request, and strictly better than blocking everything.

**B — Cheaper probe.** The subprocess only needs the package dir. `python3 -c
"import importlib.util; print(importlib.util.find_spec('ansible').origin)"` finds it
without importing the package (~0.1 s cold vs 3.6 s), or derive it from the `ansible`
shim's shebang with no subprocess at all. `install.rs` already has fast paths; this extends
them so the CLI fallback almost never runs.

**C — Persist the result.** Cache `package_dir` on disk keyed by the ansible binary's path
+ mtime; validate cheaply on startup, re-detect only when it changes. Makes every start
warm, including the first after boot.

## Done when

- [x] startup log line reports detect duration and which path won (instrument first — this
      is the missing metric that hid the cost)
- [x] the pump is never held by detect: features respond in <100 ms after `initialized`
      even with cold caches and Ansible installed
- [x] "Ansible not found" toast and status-bar state still arrive when detection fails
      — moved with detect into the detached task, ordering unchanged

## What shipped

Option A. `initialized` now logs `ready` and spawns `Backend::startup`, which detects, logs
the detect line, sends the status notification and toast, then awaits `scan_workspace`.
Detect stays first: the scan's module resolution wants the install anyway.

The instrumentation is an `AnsibleInstall::source` (`Source` enum) plus `detect_ms`, stamped
at whichever branch found `package_dir` and reported as
`ansible-lsp detect: <path> in <n> ms — <package dir>`. `bench-t075.js` prints it too.

## What the numbers said

WSL, Ansible installed (`uv tool install ansible-core`), three window reloads:

```
detect: path-walk-up in 1 ms — /home/orhayat/.local/share/uv/tools/ansible-core/lib/python3.12/site-packages/ansible
```

1 ms, three for three, and never the subprocess. **B and C are rejected on this**: both exist
to avoid an `ansible --version` call the filesystem fast path already prevents. B optimizes a
branch that isn't taken; C caches a result that costs 1 ms to recompute. Neither was written.

Two corrections the log forced:

- **The 3.6 s is macOS, not Python.** It's the kernel's first-exec security assessment
  (Gatekeeper/notarization), which is why it's 3.6 s cold and 0.35 s warm — the assessment
  caches per binary. Nothing on the fast path execs anything: `which()` canonicalizes
  symlinks and stats directories, so there is no launch for the scanner to intercept. The
  penalty is only reachable through `from_version_command`, i.e. only when every filesystem
  path has already missed. **T-075's claim that the Mac's felt freeze was cold detect is
  therefore unsupported** — it was inferred from timing the bare command, never through
  `detect()`. Corrected in that ticket; one instrumented run on the Mac would settle it.
- **A uv install resolves as `path-walk-up`, not `tool-install`.** uv symlinks
  `~/.local/bin/ansible` into the tool venv and `which()` canonicalizes, so the walk-up
  reaches site-packages and `find_tool_install()` never fires. `install.rs`'s comment
  claiming uv's shim puts the exe out of the walk-up's reach is true on Windows (a real
  trampoline `.exe`) and false on Linux; comment corrected.

Residual, accepted: if the subprocess branch *is* reached on macOS it still costs ~3.6 s.
It now runs detached, so the cost is a late toast rather than a dead editor — which is what
A was for.

## Post-close bench (2026-08-04, M3 Pro, after the bare-name/rename-table work)

CLI `scan` (includes detect + the runtime.yml table parse), first run vs repeats:

```
scan demo              (63 files)   0.95 s cold   0.02–0.04 s warm
scan ~/app/ansible  (731 files)  4.98 s cold   ~1.5 s warm
ansible --version                   2.81 s        (the subprocess detect never runs)
```

- The cold/warm split is filesystem cache, not code — nothing left for the rejected B/C.
- In-editor line the same day: `60 files analysed of 63 seen in 67 ms` with the pump free —
  the intended end state. The logged phases (parse 1, context 1, var-index 35, resolve 7)
  sum to 44 ms; the ~23 ms remainder is walk/IO with no phase bucket.
- `ansible --version` on WSL reported at ~0.1 s: the fallback's cost spans 30× across
  platforms (Linux cheap, macOS brutal, Windows crashes) — confirming last-tier is the only
  sane place for it.

## Refs

Found while measuring T-075 (its ticket has the numbers). T-074's scan metrics are blind to
this by design — the scan line starts after detect. Landed on top of T-075's `Arc<State>`
split. The startup time that's actually left is all T-076: var-index is 85% of the scan.
