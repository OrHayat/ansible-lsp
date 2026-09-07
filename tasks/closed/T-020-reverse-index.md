# T-020 — Reverse index

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| done   | P2       | M    | T-113 | —          |

## Problem

Everything the resolver does today is forward-only: *this reference points there.* Four
separate wants all need the inverse — *what points at this file?*

- **"Who includes this?"** — open a task file and you currently cannot tell what reaches it,
  which is the question you ask before editing anything shared
- **T-021** unused-file / unused-role: a file with no inbound edges
- **T-022** circular includes: a cycle in the graph
- **T-012** file watcher: republish exactly the files affected by a change, instead of
  rescanning everything

Building it once, properly, is cheaper than four partial versions.

## Approach

The scan already resolves every reference in the workspace, so this is mostly bookkeeping:
invert the results into `target path -> Vec<(source path, span, ReferenceKind)>`.

Design points worth settling before writing it:

- **Templated references produce edges to *every* candidate.** Overcounting is the safe
  direction: it can only make a file look used when it might not be, and a false "unused"
  hint is much worse than a missed one. This is precisely the case that broke a first
  grep-based estimate — `roles/access-point/tasks/protocol-base/validate-expose.yml` looked
  unreferenced but is reached via `validate-{{ _ap_op_type }}.yml`.
- **Unparseable files contribute no edges**, so anything only referenced from one looks
  unused. T-013's hint is what makes that visible instead of silent; T-021 should also
  suppress unused-hints entirely while any file fails to parse, or state the caveat.
- Keys are resolved absolute paths, so two spellings of the same target collapse.
- Exposed as a custom request (`ansible/whoReferences`) plus a command, not
  `textDocument/references` — that request is symbol-scoped, and T-011 is the record of what
  happens when file-scoped data is forced into a symbol-scoped protocol.

## Outcome

Landed as `ansible_core::reverse` — `ReverseIndex`, keyed by canonical target, one `Edge`
per reference line per candidate — built by both consumers from the `(Reference, Resolution)`
pairs they already compute.

- **Server:** `State.reverse`, filled by the workspace scan and kept current per file by
  the didChange path. An open buffer's own analysis wins over the scan's, checked at read
  time and again at insert time — the same rule the scan's diagnostics follow. A rescan
  forgets sources it did not reach (deleted, or stopped parsing) unless a buffer owns them;
  didClose re-reads the file from disk, since the buffer's edges may never have been saved.
- **Request:** `ansible/whoReferences` `{uri}` -> `{scanning, refs: [{uri, range, kind,
  templated}]}`. `scanning` is the honesty flag: while the scan runs the list is partial,
  and the client says so instead of "nothing reaches this file".
- **Command:** `Ansible LSP: Who References This File` — a quick pick, one row per edge,
  `path:line` and the kind, templated edges marked as one of several the line may reach.
- **`scan`:** always prints `reverse index: N edges -> M targets`; `--reverse` prints every
  target and what reaches it. `demo/`: 236 edges -> 73 targets.
- **Cost:** measured with the release `scan` on `demo/`, 15 interleaved runs each: 36.5 ms
  median before, 36.4 ms after. The first cut was 46 ms — one `canonicalize` syscall per
  edge — which is why `edges_of` takes the caller's `Fs` and goes through the scan cache's
  memoized `canonical`.

What the edges say, pinned per consumer (rule 3): `reverse.rs` unit tests for the index,
`main.rs` for the scan, the edit path and the request, `scan_cli.rs` for the listing, and a
demo pin for a plain include, a templated pair and a `meta/main.yml` dependency (rule 4).
Each was watched red under a deliberate break of the code it owns (rule 5).

Not done here, on purpose: T-021 / T-022 / T-012 consume this; none is started.

## Done when

- [x] a command on any task file lists every reference reaching it, with line numbers
- [x] templated references contribute an edge per candidate
- [x] role dependencies (T-018) count as edges
- [x] `scan` can print the index, so it's inspectable without an editor
- [x] building it adds no measurable time to the existing scan
