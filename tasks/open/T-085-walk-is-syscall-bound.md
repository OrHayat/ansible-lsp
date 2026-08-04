# T-085 — The var walk is syscall-bound, and 4× of the syscalls are repeats

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | M    | T-076 (landed) |

## Problem

T-076 removed the redundant *walking* — a shared subtree is now read, parsed and walked once
per pass instead of once per consumer (346 edges collapse to 61 files on the demo). What's
left is not redundant walking, it's **redundant syscalls inside those 61 walks**, and on a
9p/network filesystem that is the whole cost.

`strace -f -c -w` on `scan demo`, WSL binary on ext4, workspace on `/mnt/c`:

| syscall | calls | errors | µs/call | share of syscall time |
| ------------ | ----: | -----: | ------: | --------------------: |
| `statx`      | 1455  | **724** | 621    | **56.9%**             |
| `openat`     |  294  |   45   | 834     | 15.4%                 |
| `readlink`   |  712  | **711** | 304    | 13.6%                 |
| `getdents64` |  243  |    —   | 263     | 4.0%                  |
| `read`       |  251  |    —   | 173     | 2.7%                  |

**2340 path touches, 538 distinct — 4.3× redundancy.** Per syscall: `statx` 1332/459 (2.9×),
`readlink` 712/147 (4.8×).

Why it only shows up on some machines — one `stat`, measured three ways:

| filesystem | hit | miss |
| ---------- | --: | ---: |
| ext4 (WSL native) | 0.61 µs | 0.63 µs |
| `/mnt/c` (WSL 9p) | 540 µs | 140 µs |

~860×. So the same code is 3.4 ms on ext4 and 1254 ms on `/mnt/c` for the same 58 files. A
Mac or a Linux checkout cannot see this bug at all; T-084 already recorded the same trap
("~40 ms on an M3 Pro, where wall clock can't measure the win").

## Where the repeats come from

Top repeated paths, from the trace:

```
123x  demo                                    find_project_root / is_role_dir walking up
 99x  site-packages/ansible/modules/debug.py  same module re-probed per consuming file
 85x  demo/tasks/sibling.yml
 79x  /mnt, /mnt/c, /mnt/c/Users, ...         canonicalize re-walking the SAME prefix
 46x  demo/ansible.cfg                        project-root probe from every directory
 28x  demo/roles/ansible.cfg
 25x  demo/roles/demo   +  25x demo/./demo    one role name, one root, 25 times
```

Three distinct causes:

1. **Role search re-probes.** `resolve::role_dir` walks `ctx.roles_roots()` calling `is_dir`
   on each candidate, and nothing remembers the answer between files. Every file mentioning
   a role re-probes the identical list — including the misses (`no_such_role` ×5 per root).
2. **The project-root walk-up.** `workspace::find_project_root` asks "is there an
   `ansible.cfg` here?" at each ancestor. T-076 memoized the finished `FileContext` per
   directory, but each *distinct* directory still re-walks the shared ancestors.
3. **`canonicalize` is per-path, and `realpath` is per-component.** 711 of 712 `readlink`
   calls return EINVAL ("not a symlink") on the shared `/mnt/c/Users/…` prefix. T-076's
   memo keys whole paths, so it never learns that the prefixes are shared.

## Options

**A — Implement `Fs` over the scan cache (recommended).** The [`fs::Fs`] trait landed with
this ticket; `StdFs` is the only implementation today and nothing caches. Add the second one:

1. memoize `kind()` **including negatives** — 724 of 1455 probes are misses, so a hits-only
   cache leaves more than half the prize;
2. resolve `canonical()` **per directory** — `canonical(dir)` memoized recursively, then one
   `lstat` on the final component, falling back to full `realpath` only when that component
   really is a symlink. ~9 syscalls → 1 for a new file in a known directory;
3. thread `&dyn Fs` into `resolve.rs`, `workspace.rs`, `glob.rs` behind `x()` / `x_in()`
   pairs, exactly as `definitions_with_deps` / `definitions_with_deps_in` already do — so
   one-shot callers (hover, goto, `didChange`) keep a live filesystem and only the scan opts
   into the memo.

- Expected: 2340 → ~538 touches, so var-index ~1160 ms → ~270 ms on `/mnt/c`. Against
  T-074's 5183 ms baseline that is ~19×, which is the order of magnitude T-076 asked for and
  did not reach (it got 3.6×).
- Also lands **T-077**: a fake `Fs` is exactly what those tests need, and `include_vars`
  already has the `MemFs` to copy.

**B — Three targeted memos, no plumbing.** Cache existence inside `role_dir`, cache the
ancestor walk inside `find_project_root`, fix `canonicalize` to be per-directory.

- Pro: roughly a third of the diff for most of the win.
- Con: three private caches with three lifetimes and no shared invalidation story, and the
  measurement stays un-instrumentable — which is how T-076's first pass got a 4× wrong
  answer (counters on 2 of ~8 call sites said 561 probes; `strace` said 2340).

**C — Shrink the candidate lists.** Fewer probes rather than cheaper ones. Real, but it
changes resolution semantics, which is a correctness surface, not a perf one. Not now.

## Done when

- [ ] `statx` and `readlink` counts on `scan demo` drop to roughly their distinct-path counts
- [ ] var-index on a `/mnt/c` workspace drops by an order of magnitude against T-074's 5183 ms
- [ ] the corpus resolves identically — `scan` output byte-identical bar counter lines
- [ ] the counters come from the `Fs` seam, not from hand-placed instrumentation

## Refs

Split out of T-076, which fixed the redundant walking this sits underneath. The `Fs` trait
(`crates/ansible-core/src/fs.rs`) landed separately as the seam this needs; it subsumed the
narrower `include_vars::Fs`.

Measure with `cargo test --release var_walk -- --ignored --nocapture` (`T076_ROOT` picks the
tree) and `strace -f -c -w`. **Bench on a 9p/network path** — on ext4 the entire phase is
3.4 ms and every change looks like noise.
