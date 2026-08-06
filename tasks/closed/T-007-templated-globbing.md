# T-007 — Templated path globbing

| Status | Priority | Size | Commits          | Epic  |
| ------ | -------- | ---- | ---------------- | ----- |
| done   | P2       | M    | 9f33fe1, 0135475 | T-120 |

## Problem

`include_tasks: "{{ protocol }}_target/check.yml"` has no statically knowable target, but the
set of files it *could* reach is knowable and small. Refusing to navigate at all is worse
than offering the candidates.

## Outcome

`glob.rs` rewrites `{{ … }}` to `*` and matches across the full search path, not one
directory. Templated references never warn — a variable can expand to anything at runtime, so
absence proves nothing.

Two guards that matter:

- **Require literal text to anchor on.** `"{{ anything }}.yml"` deliberately matches nothing;
  without a guard it becomes `*.yml` and links every task file in the role. First cut
  stripped `.`/`/` and counted `"yml"` as 3 literal characters, so it matched everything —
  the anchor check now strips the extension before counting.
- **Return every candidate, not the first.** Cmd+click on a 2-match glob was landing on one
  file only, because a `documentLink`'s target overrides the definition provider. Links are
  now emitted only when there's exactly one target; multi-target navigation goes through
  `definition`, which VS Code renders as a peek list.

`ImportPlaybook` is excluded from the templated skip — see T-009.
