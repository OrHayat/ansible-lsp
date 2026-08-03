# T-071 — `unconstrained-path-var`: surface the value set a templated path implies

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P3       | S    | —          |

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

**Later, once a constraint is readable** (T-032 static `when:`, T-041 `choices:`): the hint
gains a mismatch mode — constraint allows `iscsi` but no `iscsi_target/` exists. Both sides
written down makes that closer to warning-worthy; severity decided when it lands.

## Done when

- [ ] `demo/tasks/main.yml:81` gets the INFO hint naming `http`, `ftp`
- [ ] no hint when a pattern matches nothing, or has no literal anchor
- [ ] `# noqa: unconstrained-path-var` works, rule id matched exactly
- [ ] overlap with T-061 checked: a variable surfaced there isn't double-reported here
