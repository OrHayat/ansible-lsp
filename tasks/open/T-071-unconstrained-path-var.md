# T-071 — `unconstrained-path-var`: surface the value set a templated path implies

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P3       | S    | T-112 | —          |

Born from a T-029 hover discussion: the disk defines a contract nothing documents.

## Problem

```yaml
- include_tasks: "{{ protocol }}_target/check.yml"
```

The glob matches only `http_target/` and `ftp_target/`, so any run where `protocol` is
anything else dies mid-run with "file not found." That constraint — `protocol` must be one
of `http`, `ftp` — is real, derivable, and written nowhere. Same shape as T-061's `-e`
contract: an input requirement the code implies but never states.

## Approach

**INFO hint, never a warning.** Templated paths never warn (the T-007 rule: absence proves
nothing) and correct code trips this — the target may be generated before the include runs,
the value may be validated by the caller, by an `assert`, or by `meta/argument_specs.yml`
`choices:` (T-041), none of which we read today.

Hint on the reference, only when the pattern has a literal anchor and ≥1 match:

> `protocol` must be one of `http`, `ftp` — the only values for which
> `{{ protocol }}_target/check.yml` exists; nothing constrains it.

Zero matches stays silent (that's T-007's never-warn case, not a contract). All-wildcard
patterns (`{{ anything }}.yml`) offer no candidates and therefore no hint. `# noqa:
unconstrained-path-var` to silence.

**Caveat on the derived set:** the glob expands `{{ x }}` within a single path component,
but at runtime the value may contain separators (`../../http/foo`), reaching files the
search never saw. So "must be one of http, ftp" is really "one of http, ftp, or any
path-shaped value that happens to land on a file" — an under-approximation. One more
independent reason this hint can never be a warning on its own.

**Later, once a constraint is readable** (T-032 static `when:`, T-041 `choices:`, T-180 an
`assert` that dominates the use) and the domain is **closed** with plain-name values, the glob is replaced by exact substitution of
each allowed value, and a severity ladder falls out:

- every value misses → the include fails for every permitted input. Equivalent to a
  literal missing path, which already warns — so **WARNING**, same rule, `# noqa` for the
  generated-at-runtime escape, listing the paths checked per value.
- some values miss → broken for some inputs: the mismatch case, naming the failing values.
  Severity decided when it lands.
- an open constraint (`!=`, a regex) or a domain value containing `/` reopens the
  unbounded set → back to offer-only, INFO at most.

**Constraint scope:** only a constraint that dominates *this* include counts — a `when:`
on the include itself, an enclosing block/play, or `argument_specs` `choices:` on the role
being entered. A same-named guard on unrelated tasks proves nothing about the value here.

**Strict mode (via T-025):** "every path variable must be constrained, declared, or
noqa'd" is a legitimate per-project discipline, but not the default — inside a role,
"unconstrained" is indistinguishable from "this role's input parameter," so a default
warning would flag every parameterized include everywhere. This rule is a designated
candidate for T-025 severity promotion: a committed project config sets
`unconstrained-path-var: warning` and gets the strict regime where it was chosen.

## Done when

- [ ] `demo/tasks/main.yml:81` gets the INFO hint naming `http`, `ftp`
- [ ] no hint when a pattern matches nothing, or has no literal anchor
- [ ] `# noqa: unconstrained-path-var` works, rule id matched exactly
- [ ] overlap with T-061 checked: a variable surfaced there isn't double-reported here
