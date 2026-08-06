# T-127 — A completion provider

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-124 | —          |

## Problem

There is no `completionProvider` in `ServerCapabilities` and no handler. Four tickets want
one and each would otherwise build its own:

| Ticket | Wants to complete |
| ------ | ----------------- |
| T-041 | role parameters from `meta/argument_specs.yml` |
| T-057 | module parameters from `DOCUMENTATION`, and `register` sub-keys from `RETURN` |
| T-115 | filter, test and lookup names |
| T-107 | keywords legal in the current context |

Every one of those is an index we are building anyway for diagnostics. Completion is the
cheapest possible second consumer, and for two of them it is the *safer* first shipping
target: T-115 makes the case that an incomplete name index is fine for completion — a missing
suggestion — and unacceptable for a diagnostic, where a missing name is a false error on
working code.

So this is not a feature that competes with the diagnostics; it is where several of them
should land first.

## Approach

Provider plus dispatch on YAML position, then one contributor per data source, added as each
index lands. The position logic is the shared work: knowing whether the cursor is on a
keyword, a module name, a parameter key, or inside `{{ }}` needs the AST from T-044 and the
same context that T-107 computes for per-class keyword sets.

Deliberately not in scope: snippets, or completing values. Keys and names only, until there is
a reason to go further.

## Done when

- [ ] `completionProvider` is advertised, with sensible trigger characters
- [ ] context detection distinguishes keyword / module / parameter / inside-`{{ }}`
- [ ] at least one contributor ships with it, so the plumbing is proven
- [ ] adding a contributor does not require touching the dispatch
- [ ] it degrades to nothing — never a wrong suggestion — when an index is unavailable
