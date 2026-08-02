# T-055 — Performance: cache the cross-file variable index; fix per-call line scans

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| done   | P2       | M    | T-048, T-050, T-052 |

## Problem

The variable features are correct but recompute too much:

1. **`vars::definitions` runs on every request.** It walks the include/role graph, reading and
   parsing every reachable file — and it's called from:
   - `ansible/references` (the paint pass) on **every debounced repaint**,
   - `hover` on every hover,
   - `goto_definition` on every jump.

   So a keystroke on a playbook that includes N roles re-reads and re-parses those N files. The
   in-file paint was cheap; the cross-file version (T-050 follow-up) traded that for disk I/O
   per repaint. Bounded by `MAX_DEPTH` and the cycle guard, but still wasteful.

2. **`line_of` is O(file length) per call.** In `variable_hover_at` it scans from the start of
   the file for *each* definition line, so a hover with several defs rescans repeatedly. A file
   read once should build its line index once.

## Approach

- **Cache** the cross-file definition set per file, keyed by document version, invalidated on
  `did_change` — mirror the existing `mutations` cache on `Backend`. The paint pass and hover
  then read a memoised index instead of re-walking. Consider caching parsed external files too
  (they change rarely) with an mtime check.
- **Line index**: build one `Document` (or a `line_starts` vec) per file when it's first read
  and reuse `byte_to_lsp`, instead of `line_of` rescanning. The per-file text cache the hover
  already builds is the place to hang it.
- Optional: share one cross-file walk between the paint pass and diagnostics rather than each
  recomputing.

## Traps / limits

- Invalidation must cover **included** files changing, not just the edited one — an edit to a
  role's `defaults/main.yml` should refresh playbooks that use it. A version-keyed cache on the
  edited file alone won't catch that; an mtime/`did_change`-fanout or a coarse "clear on any
  change" is the honest first cut.
- Don't cache across a workspace scan that already invalidates wholesale (see how `mutations`
  is cleared).

## Progress

The **dependency-tracked cache** landed (commit `389f5dc`): `vars::definitions_with_deps`
returns the files a walk read; the LSP caches per path with a reverse map `file -> dependents`
and invalidates precisely on `did_open`/`did_change`.

The **line-index fix** landed too: `Document::line_of` now does a `partition_point` over the
precomputed `line_starts` (O(log lines)), replacing the O(file-length) `line_of` free function.
Both hover paths (`variable_hover_at`, `path_substitution_hover`) cache external files as
`Document`s and reuse their index across every definition shown. Ticket complete.

## Done when

- [x] `definitions` results are memoised so a repaint doesn't re-walk unchanged files
- [x] editing an included var file refreshes dependent files' variable links (reverse map)
- [x] `line_of`-style per-call rescans replaced by a per-file line index
- [x] no behaviour change — same links, hovers and jumps, just fewer file reads
