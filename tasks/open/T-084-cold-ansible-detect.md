# T-084 — Cold `ansible --version` blocks startup for seconds

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | S    | —          |

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

- [ ] startup log line reports detect duration and which path won (instrument first — this
      is the missing metric that hid the cost)
- [ ] the pump is never held by detect: features respond in <100 ms after `initialized`
      even with cold caches and Ansible installed
- [ ] "Ansible not found" toast and status-bar state still arrive when detection fails

## Refs

Found while measuring T-075 (its ticket has the numbers). T-074's scan metrics are blind to
this by design — the scan line starts after detect. Fix lands on top of T-075's
`Arc<State>` split.
