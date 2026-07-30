# T-014 — README is stale

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| done   | P1       | S    | —          |

## Problem

Known-wrong statements in `README.md`:

| Says                                          | Actually                                          |
| --------------------------------------------- | ------------------------------------------------- |
| `cargo test  # 37 tests`                      | 44 tests                                          |
| `client/  ~50-line VS Code extension`         | 119 lines                                         |
| "(soon) broken-reference diagnostics"          | shipped in T-006                                   |
| "Remaining: … the execution tree (`callHierarchy`)" | rejected — T-011                              |
| "Steps 0–4 of the plan"                       | the plan's step list is superseded by this board   |
| **"press F5"** to launch the dev host         | see below                                         |

The F5 line is the one that cost real time: on this Mac F5 is the mic/dictation key, so it does
nothing at all in VS Code. The working instruction is **Run -> Start Debugging**.

P1 because the F5 line cost a real debugging session, and because a README that's wrong about
the test count is a README nobody trusts about anything else.

## Approach

Fix the facts. Point *Status* at `tasks/README.md` rather than restating a snapshot, so this
can't drift the same way again — the board is the single place status lives.

Drop the reference-kind table's overlap with the board and keep the README to: what this is,
layout, how to build, how to run it in VS Code, the deliberate silences, and the two findings.

## Done when

- [ ] no factual claim in `README.md` is false
- [ ] `Status` links to the board instead of duplicating it
- [ ] the launch instruction says Run -> Start Debugging
