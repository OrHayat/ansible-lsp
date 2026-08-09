# T-148 — Audit how ansible-core validates meta/runtime.yml, then mirror it

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P3       | M    | —          |

## Problem

A collection's `meta/runtime.yml` (`requires_ansible`, `plugin_routing`, `action_groups`,
`import_redirection`) is the one metadata file we already *consume* — `resolve.rs:690-700`
reads `plugin_routing` to follow module renames — but nobody validates it. Unlike
`meta/main.yml` this is **not** the `FieldAttribute` machinery: T-107's transcription and
oracle do not apply, and it is unaudited what ansible-core itself does with an unknown
top-level key, a typo'd `plugin_routing` entry, or a bad `requires_ansible` — it may well
be lenient-by-loader, in which case an ERROR from us would be a lie. ansible-lint carries
a JSON schema for the file, but that is lint's contract, not core's.

## Approach

Audit before any rule, same discipline as T-107: find the actual load path in the source
checkout (`utils/collection_loader/_collection_finder.py` and whatever consumes
`_meta` from there), read it whole, and record per key what core does — reject, warn,
ignore. Only what core enforces becomes a diagnostic; anything lint-only is at most a
hint, clearly separated. If the audit shows core enforces nothing, the honest outcome is
to record that and reject the rule half of this ticket.

## Done when

- [ ] the load path is read whole and the per-key behaviour table is in this ticket,
      with file:line cites
- [ ] decision recorded: which keys (if any) core actually rejects
- [ ] if a rule is warranted: unknown/invalid keys flagged at the severity core's
      behaviour justifies, `# noqa`-suppressible, zero false positives on the demo
      collections
- [ ] if not: ticket closed `--rejected` with the audit as the record
