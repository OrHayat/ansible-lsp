# T-215 — requirement() drops matches_default so an All clause that runs by default reads as required

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P2       | S    | T-121 | —          |

## Symptom

A multi-clause `when:` whose first readable clause runs by default is still rendered as a
requirement. Measured through `classify_all` + `label()`:

| `when:` clauses | rendered |
| --- | --- |
| `mode \| default('a') == 'a'` + `other is defined` | `runs only if mode = a +1 more` |
| `m \| d('a') in ['a','b']` + `other is defined` | `runs only if m in [a, b] +1 more` |

In both rows the named clause is satisfied when the variable is **unset** — that is what
`matches_default` records — so "runs only if mode = a" is false. The task also runs with `mode`
unset, and the hint gives the reader no way to know.

## Cause

`Verdict::label()` uses `matches_default` to pick "runs unless" versus "runs only if".
`Verdict::requirement()` does not look at it at all:

```rust
Verdict::WhenEquals { var, value, negated: false, .. } => format!("{var} = {value}"),
Verdict::WhenIn { var, values, negated, .. } => format!("{var} {}in [{}]", ...),
```

`Verdict::All`'s label then wraps whatever comes back in `runs only if {first} +{extra} more`,
so a clause that runs by default is presented under the one framing that excludes it.

The `..` in those arms is deliberate today — `requirement()`'s doc says it "deliberately drops
the 'runs unless' framing — that describes a whole condition, and a clause ANDed with others
does not describe the whole condition." That reasoning is right about *framing* and wrong about
*content*: `All`'s own label supplies the framing, and it supplies the wrong one.

## Not T-213

T-213 gave `WhenIn` a `matches_default` and fixed `label()`. This is the same field being
ignored by a different reader, and it was already true of `WhenEquals` before T-213 existed —
the first row above does not involve `WhenIn` at all. Filed separately so the fix is not
mistaken for a consequence of that one.

Rule 3 is the frame: the rule about what a default means belongs on the verdict, and every
reader has to answer from it. There are two readers and only one of them does.

## Fix

Either `requirement()` distinguishes the two — `mode = a (or unset)` — or `All::label()` stops
using "runs only if" when any named part runs by default. The second is smaller and keeps
`requirement()` a bare phrase, which is what its callers want.

Whichever is picked, `All` must also account for the *unreadable* clauses it folds into
`+N more`: a condition that runs by default in its readable half can still be gated by the
half we cannot read, so the wording must not promise a default run either.

## Done when

- [ ] the two conditions in Symptom render something that is true of an unset variable, with
      both asserted verbatim
- [ ] a control: the same shapes with a default that does *not* satisfy the clause still read
      as a plain requirement, so the fix reads the field rather than always hedging
- [ ] `WhenEquals` and `WhenIn` are both covered — this is one rule with two verdict types
- [ ] the `+N more` case is asserted, not only the single-part case, since that is where the
      unreadable clauses hide
- [ ] measured against ansible-core, not reasoned: a playbook where the named clause is
      satisfied by the default and the task genuinely runs
