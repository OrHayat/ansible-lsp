# T-001 — YAML crate spike

| Status | Priority | Size | Commit |
| ------ | -------- | ---- | ------ |
| done   | P1       | S    | pre-history (spike) |

## Problem

Everything downstream depends on a YAML parser that keeps source positions. Pick one before
writing code against its API.

## Outcome

Ran three candidates over all 734 `.yml`/`.yaml` files in `~/matrix/ansible`:

| Crate             | Parsed  | Time  | Verdict                                          |
| ----------------- | ------- | ----- | ------------------------------------------------ |
| **saphyr 0.0.11** | 733/734 | 38 ms | **chosen** — `MarkedYaml` is a tree with spans   |
| yaml-rust2 0.11   | 733/734 | 34 ms | same parser, but the loader discards markers     |
| marked-yaml 0.8   | 129/734 | 29 ms | eliminated — demands a mapping at the top level  |

marked-yaml is out because Ansible task files are top-level *sequences*; it fails 605 files
with `Top level must be a mapping`.

saphyr over yaml-rust2 because finding `tasks_from`'s sibling `name:` is a key lookup on one
mapping node — exactly the flow-mapping case the legacy plugin's line scan gets wrong.

Cost accepted: saphyr is pre-1.0, so it's pinned `=0.0.11` and `parse.rs` is the only module
that touches it. Migration stays a one-file change.

Two findings from the spike outlived it and are recorded in the board's *Settled* table:
character-offset markers, and PyYAML being laxer than YAML 1.2.
