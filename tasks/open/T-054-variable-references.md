# T-054 — Find variable references (the reverse of go-to-definition)

| Status | Priority | Size | Epic  | Depends on   |
| ------ | -------- | ---- | ----- | ------------ |
| open   | P3       | M    | T-113 | T-049, T-020 |

## Problem

Go-to-definition answers "where is this variable set." The inverse — "where is this variable
*used*" — is what you need before renaming or deleting a var, or when judging whether a
`set_fact` still matters. Nothing answers it today.

## Approach

`vars::uses` already extracts every use with a span in one file. Lift it workspace-wide: scan
the project's YAML files (the reference resolver already walks them — reuse T-020's reverse
index if it lands), collect uses by name, and expose them as `textDocument/references`.

Unlike file references (T-011's cautionary tale), a variable *is* a symbol-shaped thing, so the
standard references request fits here — no protocol-abuse.

## Traps / limits

- Variable scope is real: a `set_fact` in one play doesn't define a var for an unrelated one.
  A first cut can be name-global (every use of the name, workspace-wide) and say so; scoping is
  a refinement, not a blocker.
- Templated names (`{{ ('a_' ~ x) }}`) can't be matched — skip, don't guess.
- Magic vars would swamp the results — offer them but low-rank, or exclude by default.

## Done when

- [ ] find-references on a variable lists every use across the workspace, with locations
- [ ] the variable's own definition sites are included (or clearly separated)
- [ ] templated/computed names are skipped, not mis-matched
- [ ] `scan` can print the use index so it's inspectable without an editor
