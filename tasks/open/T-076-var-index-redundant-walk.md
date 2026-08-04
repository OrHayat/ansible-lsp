# T-076 — Var-index re-walks shared files once per consumer

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | M    | —          |

## Problem

The startup scan spends ~85% of its time in variable indexing — 4835 ms of 5674 ms on a
56-file demo (T-074), ~86 ms/file on tiny YAML where actual parsing is 3 ms *total*. It is
redundant work, not real work.

`definitions_with_deps` → `collect` (`vars.rs`) does a **full transitive walk per file**: for
each file it re-runs `FileContext::discover` (another `ansible.cfg` read) and follows every
include / role / meta-dependency edge, reading + parsing + `ast::build`-ing + `resolve::resolve`-ing
each target from disk. The `visited` dedup set is **per file**, so a shared file (role
`defaults`/`vars`/`meta`, a shared task file, group_vars) is walked once *per consumer*. In the
demo everything routes through `tasks/main.yml`, so the same subtree is re-walked ~56 times.
It's O(files × subtree). The per-keystroke `VarCache` (T-055) caches the top-level result but
shares nothing *between* files during the cold startup pass.

## Options

**A — Full memoization (recommended).** A scan-scoped cache with three layers:

1. parsed documents by canonical path (`path -> Arc<[Node]>`), so each on-disk file is read +
   parsed at most once per scan;
2. `FileContext` / `AnsibleConfig` by directory, so `ansible.cfg` is read once per dir, not
   once per file;
3. **the expensive one** — memoize each file's contributed definitions keyed by canonical
   path, so a shared subtree's `ast::build` + `index` + edge `resolve` runs once total.

- Pro: removes the root cause; the O(files × subtree) blow-up collapses to O(files). Expect
  10–50× on the var-index phase.
- Con / risk: `collect` stamps provenance (`via` chains, T-066) and `when:` conditions onto
  defs *in the caller's context*, so layer 3 must cache the raw per-file contribution and
  re-stamp `via`/`condition` at merge — not cache the finished, already-stamped list. Get that
  wrong and a def is dropped or mis-attributed. Must keep the corpus and the T-066 hover-
  provenance tests green (`hover_breadcrumbs_meta_dependency_routes_only`, the var perf/corpus
  tests). Cache is scan-scoped and rebuilt each pass, so no staleness across edits.
- Size: ~half day.

**B — Safe caches only.** Layers 1 and 2 above; leave the walk structure (layer 3) alone.

- Pro: low risk — walk logic untouched, var results byte-identical to today, cache rebuilt
  per scan. ~1–2 hours.
- Con: partial. It dedups reads/parses and `ansible.cfg`, but the walk still re-runs
  `ast::build`, `index`, and `resolve::resolve` (filesystem stat/canonicalize) per consumer,
  which is a large share of the cost. Estimate ~2–3×, not 10×. Leaves the quadratic structure.

## Recommendation

A. B is the fallback if A's provenance re-stamping proves too fiddly to land safely; ship B
first as a stepping stone if needed, then layer 3 on top.

## Done when

- [ ] the var-index phase (T-074's log line) drops by an order of magnitude on the demo
- [ ] each on-disk file is read + parsed at most once per scan
- [ ] `ansible.cfg` is read at most once per directory per scan
- [ ] variable hover/goto and the T-066 provenance breadcrumbs are unchanged (tests green)
- [ ] the corpus scan (`~/app/ansible`) resolves identically to before

## Refs

Surfaced and quantified by T-074. Pairs with T-075 (backgrounding) — that stops the freeze,
this stops the wasted work; do both.

Re-confirmed post-T-075 on WSL: **var-index 5183 ms of a 6113 ms scan (85%), parse 2 ms** —
the same ratio as T-074's original numbers, so backgrounding changed nothing about the cost.
Note the platform gap when benching: this phase is ~40 ms total on an M3 Pro, where wall clock
can't measure the win at all. Use WSL, or the walked-vs-unique-files counter.
