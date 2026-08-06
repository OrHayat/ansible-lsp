# T-049 — Variable uses with byte-accurate spans

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| done   | P2       | M    | T-112 | —          |

## Problem

Go-to-definition and definedness both need the inverse of the definition index: every place a
variable is *used*, with an exact span so the cursor can be matched to a name.

## Approach

- `condition::variable_uses` scans the original expression (skipping string literals in place,
  not via the length-changing `strip_strings`) and returns each root variable with its byte
  range; `condition::variables` now reuses it.
- `vars::{template_uses, expression_uses, uses}` lift that to whole files — a `when:` value is
  one expression, any other scalar is literal text with `{{ }}` islands.

Gated by the `scan` snapshot (the mutation rule calls `condition::variables`).

## Done when

- [x] uses extracted from `{{ }}` templates and bare `when:` expressions
- [x] spans byte-accurate past non-ASCII (literals skipped in place)
- [x] `scan` output byte-identical after the refactor

Resolution: commit `85d0d6f`.
