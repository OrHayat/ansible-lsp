# T-129 — Triage every ansible-lint rule: covered, port, reject, out of scope

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-128 | —          |

## Problem

ansible-lint is the tool this project's users already run, and in ~200 commits the board has
never once compared itself to it. The single mention anywhere in `tasks/` is
`tasks/README.md:246`, establishing that `when-import-var-mutated` is **not** a
reimplementation — "ansible-lint has no rule for this; in 26.1.1 `import_playbook` appears
only in `fqcn.py`."

That is a good datapoint and it is also the only one. We do not know how much of what we
ship, or plan to ship, duplicates a rule a user already gets. Nor which of its ~50 rules are
cheap wins over the AST we already have.

## Approach

Go through the rule list once and put every rule in exactly one bucket:

| Bucket | Meaning |
| ------ | ------- |
| **covered** | we already diagnose it — record which rule id, so overlap is visible |
| **port** | cheap over the existing AST, and better in an editor than in CI |
| **reject** | style or opinion, not a fault — with the reason, so it stays rejected |
| **out of scope** | needs a runtime, a git history, or a galaxy round-trip |

Deliberately **one** ticket and not fifty. Filing per-rule tickets before the triage would be
inventing work: the triage decides how many children this epic actually gets, and "port" is
expected to be a minority bucket.

Two things to settle while reading, because they shape everything after:

- **Where we should differ.** ansible-lint is a CI tool: it can be strict, noisy, and
  config-driven because a human reads the output once per PR. An editor cannot. A rule that
  is right for CI and wrong for a squiggle belongs in **reject** with that as the reason.
- **What we can do that it cannot.** We hold a resolved cross-file graph and a variable
  index. Rules that need to know where a file or variable actually came from are ours to
  have and are not portable to a per-file linter — that is the same argument
  `when-import-var-mutated` already proved.

Candidates that look like `port` before reading: `no-free-form`, `risky-file-permissions`,
`partial-become`, `no-handler`, `deprecated-module`, `key-order`. Not a commitment.

## Done when

- [ ] every ansible-lint rule is in exactly one bucket, with the version triaged against
- [ ] each **covered** entry names our rule id, so duplication is visible
- [ ] each **reject** entry carries its reason, in the style of the board's rejected tickets
- [ ] the **port** bucket becomes children of T-128 — one ticket per cluster, not per rule
- [ ] the result lands in the epic, not in a scratch file
