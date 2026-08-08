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

`resolve.rs` is **done**: all 18 `repo() else { return }` guards are gone and the `repo()`
helper with them, so nothing in that file reads `$HOME` any more. 16 became `MemFs` trees; the
two install-gated ones kept only their install check (below). A `mem_in` helper sits beside
`mem_src` for the four magic-var cases, which call the local two-argument `resolve_in`.

Two of the 16 still fail when un-ignored, and correctly so: `role_path_expands_to_the_containing_role`
and `an_expanded_path_that_is_missing_still_warns` return `Skipped` because `role_path`
substitution is switched off at `resolve.rs:138` pending T-067/T-068. The fixture is right and
the feature is absent — they go green when that lands, with no second pass needed.

The port also exposed a hole in `MemFs`: `read_dir` reported only files, never subdirectories.
`glob::expand` filters that listing on `Kind::Dir` to descend a wildcard segment
(`glob.rs:80`), so **every `*/name.yml` pattern silently matched nothing** and any test globbing
across a directory segment would have passed vacuously. Fixed in `testing.rs`.

### The corpus replacement

The two corpus tests couldn't become hand-written fixtures — a sweep asserting "we don't
misjudge real conditions" proves nothing against conditions you wrote yourself. They now run
against **official collections** instead of one private repo:

| Collection (`ansible-collections/…`) | Commit    |
| ------------------------------------ | --------- |
| `ansible.posix`                       | `ffdf9ef` |
| `community.general`                   | `6d6d64d` |
| `community.docker`                    | `bc8f6a6` |
| `community.crypto`                    | `498036f` |

1941 YAML files, 1211 `when:` sites, 1313 clauses, 416 distinct expressions. `condition.rs`'s
`corpus` module inlines the shape-diverse subset (86 expressions, verbatim, YAML quoting
stripped) as `REAL_WHENS`, and `mutation.rs` reproduces `ansible.posix`'s selinux include chain.

That immediately paid for itself: the sweep found **3 false positives**, all
`when-item-without-loop`, all in files included with the loop at the include site — filed as
**T-139** (P1) and pinned by a test that asserts today's wrong answer so the fix has somewhere
to land.

Coverage measured at 11/86 classified, 7/86 guarded — in line with the full corpus (166/1313,
12.6%), so the sample is representative rather than cherry-picked. Both are floors now, not
printouts: the old test was `#[ignore]`d and only ever printed percentages.

**Remaining: 1 site** — `resolve.rs`'s `perf::profile_largest_file`, which times the biggest
file it can find. A fixture cannot stand in for "the biggest file in a real tree", so give it
the `ANSIBLE_CORPUS` env var `parse_libyaml::corpus_smoke` already uses.

Note `builtin_modules_resolve_into_the_installed_ansible` and
`installed_collection_modules_resolve` are a *different* dependency — the machine's Ansible
install, not the repo — and are out of scope here. Their `repo()` guard was still dropped: an
empty `testing::project` is enough, since nothing under the project root decides where
`ansible.builtin` lives, so they now skip for the one reason they actually have.

## Done when

- [ ] no test reads `$HOME` / a hardcoded private repo path
- [ ] each moved test builds its tree inline and runs (not skips) on a clean checkout and CI
- [ ] `smoke.js` / `timing.js` either take a path argument or build a temp tree inline
