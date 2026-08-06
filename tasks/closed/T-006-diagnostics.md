# T-006 — `missing-file` diagnostics + repo-wide scan

| Status | Priority | Size | Commit  | Epic  |
| ------ | -------- | ---- | ------- | ----- |
| done   | P1       | M    | 9f33fe1 | T-090 |

## Problem

The original ask: *warn if a file doesn't exist.* Nothing does this today — you find out when
the playbook fails mid-run, possibly against a live cluster. And warnings only in open files
are near-useless, because a rename's blast radius is in files nobody has open.

## Outcome

`textDocument/publishDiagnostics` on open/change, plus a whole-workspace scan after
`initialize` responds (off the request path, so activation isn't blocked). Each diagnostic
carries `code: rule_id(r)` so it can be suppressed by name (T-010) and listed by rule.

The trust rule — only fully literal paths warn:

| Case                            | Behaviour                                   |
| ------------------------------- | ------------------------------------------- |
| literal, nothing on disk        | **Warning**, message lists every path tried |
| contains `{{ }}`                | navigation only, never warns                |
| module arg not in the table     | never warns                                 |
| absolute, outside the workspace | never warns                                 |

`bin/scan.rs` is the same core as a CLI — prints the per-kind table and exits non-zero on
missing files, so this works as a pre-commit or CI check. It's the only thing that catches
breakage in files nobody opened.

**The corpus gate held:** zero false positives across 731 files. The single missing file it
reports is a real break (T-030) — found by this ticket, in a repo that has been shipping
with it.
