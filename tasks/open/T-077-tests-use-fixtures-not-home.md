# T-077 — Real-repo tests read a caller's home dir; move to inline fixtures

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| **partly done** | P2 | M | T-130 | — |

## Problem

A batch of tests resolve against a private repo hardcoded to the caller's home —
`~/app/ansible`. They guard with `repo() else return`, so they silently **skip** on any
machine that doesn't have that exact tree. That's not shippable: coverage depends on one
person's home directory, and a green run proves nothing on CI or a fresh checkout.

Files with the pattern (`.join("app/ansible")` / `repo()` helpers):
`config.rs`, `glob.rs`, `resolve.rs`, `workspace.rs`, `mutation.rs`, and the `scripts/*.js`
harnesses (`smoke.js`, `timing.js`).

## Approach

Build each fixture **inline in the test**, not as a committed fixture directory. A small
helper writes the handful of files a case needs into a `tempfile::tempdir()` from string
literals in the test body, then points the resolver at that temp root:

```rust
let root = tree(&[
    ("roles/r/tasks/main.yml", "- include_tasks: query/exists.yml"),
    ("roles/r/tasks/query/exists.yml", "- debug: {msg: ok}"),
]);
```

Each pinned behaviour gets its own tiny tree, named for the behaviour it guards — nested
task includes resolving against the role `tasks/` dir, the `tasks_from`-without-`main` role,
the in-repo collection module, the mutated-import case. No `$HOME`, no external files to keep
in sync, and the fixture reads at the call site so a test states exactly what it depends on.

Keep each tree minimal: only the files the behaviour needs, not a copy of a real role.

## Progress

`testing.rs` holds the helper — `tree(name, files)`, `project(name, cfg, files)` — writing
string literals into a wiped temp dir. **`name` must be unique per test**: the wipe means two
tests sharing one would delete each other's files under the parallel harness.

It needed an entry in `fs.rs`'s `EXEMPT` list. The `no_filesystem_call_bypasses_the_seam`
guard skips each file's content after an in-body `#[cfg(test)]`, and `testing.rs` has none —
its `cfg` sits on the `mod` in `lib.rs` — so the whole file read as production code.

Converted so far, all now *running* rather than skipping:

| Test | Was |
| ---- | --- |
| `workspace::nested_task_file_resolves_against_role_tasks_dir` | panicked on Windows (`HOME` unset) before its own skip guard |
| `glob::a_templated_directory_segment_matches_every_sibling` | `finds_real_matches_in_the_repo`, silent skip |
| `config::a_colon_list_expands_tilde_and_dot_against_the_project_root` | `reads_the_real_repo_config`, silent skip |
| `resolve::includes_that_do_not_resolve_relative_to_the_including_file` | `real_repo_regressions`, silent skip |
| `resolve::sibling_include_inside_a_tasks_subdirectory` | silent skip |
| `resolve::missing_file_reports_every_candidate_tried` | silent skip |

Two got stronger in the move, because a real tree could only be asserted loosely: the glob
case now pins an exact count of 3 and that a non-matching sibling dir is excluded; the
`workspace` one asserts its fixture exists, so it can never silently become a no-op again.

**Write the remaining ones as `MemFs` trees, not `testing::tree`.** T-134 landed the shared
in-memory `Fs` after these six shipped as tempdirs, so these six are the rewrite it predicted;
don't add more. `tree`/`project` stay only for callers that hardcode `StdFs`.

**Remaining: 25 sites** — `resolve.rs` 23, `condition.rs` 1, `mutation.rs` 1. The `resolve.rs`
bulk is `let Some(root) = repo() else { return };` at the top of ~18 tests. Each is
self-describing (it names the paths it needs and what it expects), so the trees can be built
from the test text without access to the private repo.

Note `builtin_modules_resolve_into_the_installed_ansible` and
`installed_collection_modules_resolve` are a *different* dependency — the machine's Ansible
install, not the repo — and are out of scope here.

## Done when

- [ ] no test reads `$HOME` / a hardcoded private repo path
- [ ] each moved test builds its tree inline and runs (not skips) on a clean checkout and CI
- [ ] `smoke.js` / `timing.js` either take a path argument or build a temp tree inline
