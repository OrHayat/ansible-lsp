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

## Instrumentation

Counting is a **decorator**, not a property of each implementation. `StdFs` stays a
zero-cost unit struct — it is what every interactive path uses (hover, goto, `didChange`),
and it has nowhere to keep state anyway short of a static, which would outlive any one pass.

```rust
pub struct Counting<F: Fs> {
    inner: F,
    stats: FsStats,
}

impl<F: Fs> Fs for Counting<F> { /* tally, delegate to inner */ }
```

| to measure | wrap |
| --------------------------- | ------------------------------------- |
| the baseline, uncached      | `Counting::new(StdFs)`                |
| after the memo              | `Counting::new(ScanCache::new(StdFs))` |
| production hover / goto     | plain `StdFs` — nothing paid          |

One implementation of counting instead of one per backend, and **both sides of the A/B
measurable with the same code** — which is the half that matters, since the before-number is
the one nobody believes. It also stacks: wrapping *inside* the cache counts syscalls,
wrapping *outside* counts calls, so neither implementation has to track both itself.

Per *operation*, not aggregated — the costs differ 3× (`openat` 834 µs, `statx` 621 µs,
`readlink` 304 µs) and one combined number hides which to attack.

```rust
#[derive(Default)]
pub struct Counter {
    calls: AtomicUsize,   // times the seam was asked
    disk: AtomicUsize,    // times it actually reached the filesystem
    misses: AtomicUsize,  // kind() == None
    nanos: AtomicU64,     // time in the disk path only
}

#[derive(Default)]
pub struct FsStats {
    kind: Counter,
    read: Counter,
    read_dir: Counter,
    walk: Counter,
    canonical: Counter,
    /// The only thing needing a map, so the only thing behind a lock — and off
    /// unless asked for. Gives `distinct` and the top-N histogram.
    paths: Option<Mutex<HashMap<PathBuf, usize>>>,
}
```

**Atomics, not a `Mutex<FsStats>`.** A single lock on every filesystem call would serialise
the one thing `ScanCache` was deliberately built to allow — it takes `&self` throughout so
files *can* walk concurrently, and the sequential scan loop is the obvious next thing to
parallelise. Worse, `ScanCache` already holds a `Mutex<Inner>`, so a stats lock doubles the
locking on every op: noise against a 540 µs 9p stat, but 20–40% against a *memo hit*, which
is a ~50–100 ns hashmap lookup and precisely the fast path this exists to create.
`fetch_add(1, Relaxed)` is ~1–5 ns and lock-free; nothing branches on these mid-run, only
the totals are read at the end.

Why each earns its place:

- **`calls` vs `disk`** is the redundancy itself — the ratio that says 2340 → 538.
- **`misses`** — 724 of 1455 today. This is the metric that catches a future hits-only cache
  silently leaving more than half the prize behind.
- **`nanos` on the disk path only.** Two `Instant::now()` calls are ~40 ns against a 540 µs
  9p stat — 0.007% where it matters. Don't time memo hits: a hashmap lookup is uninteresting
  and on ext4 the timer costs more than the thing measured. This is the portable number,
  directly comparable across filesystems that are 860× apart on wall clock.
- **`distinct` and top-N repeated paths**, behind an env var, both from `paths`. `distinct`
  is the floor a memo can reach: if `calls ≈ distinct`, memoizing is the wrong fix and the
  answer is option C. The histogram is what actually diagnosed this bug — `demo/roles/demo`
  ×25 and `demo/ansible.cfg` ×28 named both causes outright. Neither can be an always-on
  counter: both need the map, and a full `HashMap<PathBuf, usize>` is too much to carry for
  a 729-file corpus on every run.

Residual, accepted: 5 operations × 4 atomics in one struct share cache lines, so heavy
multi-threaded counting gets false sharing. Unmeasurable at syscall rates; padding it would
be optimising the instrument instead of the thing.

Deliberately **not** added: per-call-site attribution. That is precisely the hand-maintained
list that drifted and produced the 4× wrong answer, just relocated.

Reporting: compact on the scan log line — `fs: 2340 calls -> 538 disk, 231 misses, 47 ms` —
and the full per-operation table in the `var_walk` perf test and CLI `scan`.

### A guard test, not a counter

A door only works if there is no window. Add a test that greps the crate for `std::fs::`,
`.is_file()`, `.is_dir()`, `.exists()` and `.canonicalize()` outside `fs.rs` and fails on a
hit. That is the structural fix for what cost the most in T-076: the counters were right
about the sites they covered and blind to the four they did not.

## Measured: five follow-up attempts, all rejected

Bench: kubespray, 584 YAML files, cloned to WSL ext4 and copied byte-identical to `/mnt/c`
(9p). 32 cores. `var_walk` for syscall counts, `parallel_spike` for wall clock. Every variant
kept the corpus identical — 27700 defs, 3257 edges -> 1276 files, 191 tests green.

| attempt | ext4 1t | ext4 32t | 9p 1t | 9p 32t |
| ------- | ------: | -------: | ----: | -----: |
| cache-pad the `AtomicStats` counters | 0 | 0 | 0 | 0 |
| `read_dir` returns `Arc<[..]>` not `Vec` | **+10%** | — | flat | — |
| `kind()` answers from an already-known parent | flat | — | −1.4% | — |
| `kind()` climbs to the nearest known ancestor | — | — | **+6.5% syscalls** | — |
| `kind()` lists the parent instead of stat'ing | +5.5% | **+26%** | **−22%** | +2% |

- **Padding.** The residual above is now measured, and it is smaller than the note assumed:
  deleting the counters *outright* — the ceiling on any counter scheme — is also unmeasurable
  (8.95 vs 9.19 ms at 32 threads, σ≈0.6–0.9). At n=5 a 1 ms artifact looked real and flipped
  sign at n=21. Interleave variants and use ≥20 reps.
- **`Arc`.** `.into_iter()` on an owned `Vec` *moves* each `PathBuf`; `.iter()` on a shared
  `Arc` must *clone* it — one allocation became N. `dirs` is rarely hit anyway, because
  `listings` and `trees` absorb the repeats.
- **Ancestor climb.** Costs one stat per unseen ancestor and only pays on a dead one. 552 live
  against 64 dead: 8.6:1 the wrong way.
- **List-the-parent.** The only one that works on its own terms — 7499 -> 5493 syscalls
  (−27%), misses 2759 -> 491, 9p sequential 17.0 -> 13.0 s. Rejected anyway: `read_dir` is far
  more CPU than `stat` (an allocation per entry plus a shard lock each), so it regresses the
  concurrent scan, and T-085's own concurrency already takes 9p sequential 19.7 -> 1.9 s. It
  competes for what threading already collected.

**The governing number is `calls ≈ distinct`.** 7499 syscalls over **5363 distinct paths**
(1.40×), and the gap is fully explained by `read`/`read_dir` touching paths that were also
`kind`-checked. By this ticket's own test (Instrumentation, `distinct`), memoizing is done —
the remaining 2759 negative probes are individual missing candidates inside *live* directories
(`roles/etcd/vars/main.yml` where the role has no `vars/`), so no prefix or ancestor trick
reaches them. What is left is **option C**, shrinking the candidate lists, with the correctness
caveat that option C already carries.

Unexplored: ext4 CPU is allocation churn, not syscalls — 27700 `Located` defs materialised for
584 files, `Contribution.defs` cloned and re-stamped per caller. Never profiled at scale; WSL
has no `perf`/`samply` and `perf_event_paranoid=2`, and Windows `cargo flamegraph` needs
`CARGO_PROFILE_RELEASE_DEBUG=true` or every frame is `Unknown` — it then mis-attributed 4.4%
to a `fetch_add` that executes twice, which is the other reason the padding question needed an
A/B rather than a profile.

## Done when

- [ ] `statx` and `readlink` counts on `scan demo` drop to roughly their distinct-path counts
- [ ] var-index on a `/mnt/c` workspace drops by an order of magnitude against T-074's 5183 ms
- [ ] the corpus resolves identically — `scan` output byte-identical bar counter lines
- [ ] the counters come from the `Fs` seam, not from hand-placed instrumentation
- [ ] no filesystem call bypasses the seam — the guard test above is green

## Refs

Split out of T-076, which fixed the redundant walking this sits underneath. The `Fs` trait
(`crates/ansible-core/src/fs.rs`) landed separately as the seam this needs; it subsumed the
narrower `include_vars::Fs`.

Measure with `cargo test --release var_walk -- --ignored --nocapture` (`T076_ROOT` picks the
tree) and `strace -f -c -w`. **Bench on a 9p/network path** — on ext4 the entire phase is
3.4 ms and every change looks like noise.
