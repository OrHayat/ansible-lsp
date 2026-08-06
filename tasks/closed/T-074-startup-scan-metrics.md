# T-074 — Startup scan metrics

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| done   | P3       | S    | T-131 | —          |

## Problem

Opening a workspace is visibly slow, but we had no in-server measurement of it.
`scripts/timing.js` times spawn → initialize → one `documentLink`; the `#[ignore]` bench in
`resolve.rs:1213` times a single file's phases. Nothing timed the real workspace scan
(`scan_workspace`, `main.rs`), and nothing reported it to the user. "Startup is slow" was a
feeling, not a number.

## What shipped

Instrumented `scan_workspace`. Split `analyze_text` into a measured variant
(`analyze_text_measured`) that accumulates per-phase durations into a `ScanTimings`; the
plain `analyze_text` passes a throwaway accumulator so the logic stays in one place. The
scan logs one line to the *Ansible LSP* output channel on startup:

```
ansible-lsp scan: N files analysed of M seen in T ms (parse …, context …, var-index …, resolve …)
```

Phases are sums across analysed files; the wall clock also covers the file walk, reads,
diagnostics and publishing, so total > the four phases.

## Research results — what the numbers said

Demo workspace (56 files), 9950X3D:

```
ansible-lsp scan: 56 files analysed of 58 seen in 5674 ms
  parse 3 ms · context 336 ms · var-index 4835 ms · resolve 215 ms
```

| Phase | Time | Share |
| --------- | -----: | ----: |
| **var-index** | **4835 ms** | **85%** |
| context | 336 ms | 6% |
| resolve | 215 ms | 4% |
| parse | 3 ms | <1% |

Findings, which redirected the whole perf effort away from the original guesses (serial
loop, per-file `ansible.cfg` reparse):

1. **var-index is essentially the entire cost** — ~86 ms/file on tiny YAML. Actual parsing
   of the top-level docs is 3 ms *total*. The 4835 ms is not real work, it's redundant work.
2. **Root cause: `definitions_with_deps` does a full transitive walk per file** (`vars.rs`).
   For each file it re-discovers context and follows every include/role/meta-dependency edge,
   reading + parsing + `ast::build`-ing + `resolve::resolve`-ing each target from disk. The
   `visited` dedup set is **per file**, so shared files (role `defaults`/`vars`/`meta`, shared
   task files, group_vars) are walked once *per consumer*. In the demo everything routes
   through `tasks/main.yml`, so the same subtree is re-walked ~56×. It's O(files × subtree).
3. **A second, independent problem surfaced: the scan blocks all requests.** `initialized`
   awaits `scan_workspace` inline (`main.rs:1193`), so tower-lsp's message pump services no
   hover/goto/`ansible/references` request until it finishes — features are dead and coloring
   is bland for the whole ~5 s. Confirmed by observation, not just reasoning.

## Follow-ups (split out)

- **T-075** — scan blocks requests during startup (problem 3). The UX fix.
- **T-076** — var-index redundant transitive walk (problems 1–2). The speed fix, two options.
