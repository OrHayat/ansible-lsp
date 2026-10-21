# T-251 — The editor's scan computes literal variable values for every file, though only a templated path reads them

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P2       | S    | T-131 | —          |

## Problem

`analyze_text_measured` called `vars::known_literals_in` for every file it analysed. That copies
out the value of every definition reaching the file, and only the resolver's substitution of a
templated reference reads the result — a file with no templated reference threw it away.
`scan` already skips it in that case (`bin/scan.rs`, "Built only when this file actually
carries a templated reference").

Profiled 2026-10-21 on the private corpus: a third of the editor scan's CPU.

## Approach

Compute the literals only when one of the file's references, `meta/main.yml` dependencies
included, is templated. The definitions themselves are still gathered: the scan fills the
variable cache that later hovers read (T-201).

## Done when

- [x] the editor scan skips the literals for a file with no templated reference
- [x] diagnostics identical before and after: published diagnostics compared file by file on
  the private corpus, community.general, debops and kubespray — identical on all four
- [x] measured: the private corpus's editor scan went from 541-600 ms to 423-447 ms, three
  runs each; community.general and debops within noise
