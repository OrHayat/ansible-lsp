# T-077 — Real-repo tests read a caller's home dir; move to inline fixtures

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P2       | M    | T-130 | —          |

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

## Done when

- [ ] no test reads `$HOME` / a hardcoded private repo path
- [ ] each moved test builds its tree inline and runs (not skips) on a clean checkout and CI
- [ ] `smoke.js` / `timing.js` either take a path argument or build a temp tree inline
