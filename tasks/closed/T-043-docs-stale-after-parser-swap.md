# T-043 — Docs stale after the parser swap

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| done   | P1       | S    | T-130 | T-036      |

## Problem

Replacing saphyr with libyaml (T-036) invalidated the docs T-014 had just fixed — including
two **"Settled — don't re-derive"** entries, the dangerous kind, since a wrong "settled" fact
gets trusted. Same class as T-014: a README nobody can trust about anything else.

Known-wrong statements:

| Says | Actually |
| ---- | -------- |
| `parse.rs` — "the only module touching **saphyr**" | parser is `libyaml-safer`, in `parse_libyaml.rs` |
| "86 tests" (README), "63 tests" (board) | 93 |
| "9 commits, ~3000 lines" | 35 commits, ~4500 lines |
| "731 files, **1 unparseable**" | 729 files, **0** — `_run.yml` parses now |
| Silence: "files that fail to parse" | now a real `unparseable` **error**, not a silence |
| Finding: "**saphyr markers are character offsets**" | libyaml marks are bytes; the char->byte step is gone |
| Finding: "unparseable must mean *no references*, never *broken*" | **reversed** — parser matches Ansible, so unparseable = broken for Ansible too |
| T-013 done but listed **open**; T-036 not on the board | moved to `closed/`, board updated |

## Approach

Fix the facts. Rewrite the two Settled findings around T-036 (the char-vs-byte trap is retired;
"match Ansible, not the spec" replaces "degrade silently"). Move T-013/T-036 to `closed/`.

## Done when

- [x] no factual claim in `README.md` / `tasks/README.md` is false
- [x] both Settled findings reflect the libyaml parser
- [x] T-013 and T-036 are in `closed/` with the board tables updated
- [x] the `unparseable`-is-a-silence framing is gone everywhere
