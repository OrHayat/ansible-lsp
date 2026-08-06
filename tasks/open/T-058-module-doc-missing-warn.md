# T-058 — Warn when a module ships no `DOCUMENTATION` / `RETURN`, with `# noqa` for legacy

| Status | Priority | Size | Epic  | Depends on   |
| ------ | -------- | ---- | ----- | ------------ |
| open   | P3       | S    | T-123 | T-057, T-010 |

## Problem

T-057 reads a module's input (`DOCUMENTATION`) and output (`RETURN`) docstrings to power
parameter/return validation. When a module **lacks** them, every feature built on top silently
does nothing — the user can't tell "this call is fine" from "the LSP is blind here." A quiet
gap reads as coverage it doesn't have.

Surface the gap: a **hint** on a task whose resolved module has no parseable `DOCUMENTATION`
(can't check args) and/or no `RETURN` (can't check a registered result's fields).

## Research (why this is common, and must be soft)

`ansible-doc` reads these docstrings and nothing else — so a module without them shows empty
docs there too. It's not rare:

- Older core modules and many **community** modules ship partial or no `RETURN`.
- Some modules document inputs but not outputs (or vice versa).
- Return docs are hand-written and can be stale — present ≠ correct.

So absence is a normal state, not an error. Any signal here is a **hint/information**
diagnostic, never a warning-that-blocks-trust. It exists to explain *why* a register's
sub-keys aren't validated, not to nag about someone else's module.

## `# noqa` — block legacy stuff

Reuse the existing rule-scoped suppression (T-010, ansible-lint syntax) so a user can silence
the hint on a known-undocumented legacy module without disabling it everywhere:

```yaml
- command: /opt/legacy/run.sh    # noqa: no-module-schema
  register: out                  # out.* won't validate; suppressed on purpose
```

New rule ids: `no-module-schema` (covers both), or split `no-module-input` / `no-module-output`
if the two need independent suppression. Compare rule ids by **exact match** — T-010's remembered
bug was `# noqa` matched as a substring and ansible-lint's own `# noqa: command-instead-of-module`
lines silenced our checks. Pin the same guarantee here.

Line-scoped and prior-line forms come free from the T-010 implementation (comments read from raw
source, not the AST).

## Traps / limits

- Never emit this as a warning/error — hint severity only. One noisy hint on every community
  task poisons the whole diagnostic set.
- Only fire once the module actually **resolved** (T-057 found the file). An *unresolved* module
  is a different problem (T-005 / missing collection), not "no schema."
- Don't fire on non-module tasks (blocks, `meta:`, pure directives).
- Consider a project-level off switch (T-025) in addition to per-line noqa, since some shops run
  entirely on undocumented internal modules.

## Done when

- [ ] a resolved module with no parseable `DOCUMENTATION` and/or `RETURN` yields a **hint** on the task
- [ ] the hint explains the consequence (args / registered fields aren't validated here)
- [ ] `# noqa: no-module-schema` (line and prior-line) suppresses it; rule id matched exactly
- [ ] never fires above hint severity, on unresolved modules, or on non-module tasks
