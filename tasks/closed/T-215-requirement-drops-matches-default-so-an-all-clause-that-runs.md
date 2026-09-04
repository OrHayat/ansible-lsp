# T-215 — requirement() drops matches_default so an All clause that runs by default reads as required

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P2       | S    | T-121 | —          |

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

## Resolution

The first option: `requirement()` reads `matches_default` and renders the hedge on the value —
`mode = a (or unset)`, `m in [a, b] (or unset)` — for both `WhenEquals` and `WhenIn`, in both
directions. `All::label()` is untouched and keeps "runs only if", which stays true: the whole
condition is still a conjunction of requirements, one of which is now stated correctly. That
also keeps the `+N more` case honest without a special case, since the unreadable clauses can
still gate the run and the wording never promises one.

Chosen over the `All`-side fix because `requirement()` has three readers, not one:
`Verdict::All::label`, `when_hover`, and `guard_line` in `main.rs`. The two hovers list each
clause as a bare requirement under "runs only when **all** hold", and they were lying the same
way. Fixing the field's reader fixes all three (rule 3); fixing `All` would have fixed one.

Measured on ansible-core 2.21.2, each condition paired with `other is defined` (`other` set):

| named clause | unset | control |
| --- | --- | --- |
| `mode \| default('a') == 'a'` | **ran** | `mode: b`: skipping; `default('b')`: skipping |
| `m \| d('a') in ['a','b']` | **ran** | `m: c`: skipping; `d('c')`: skipping |
| `mode \| default('b') != 'a'` | **ran** | — |
| `m \| d('c') not in ['a','b']` | **ran** | — |
| `mode \| default('a') == 'a'` + `gate \| int > 3` | `gate: 1`: skipping | `gate: 9`: ran |

Pinned by `a_requirement_the_default_satisfies_says_so` in `condition.rs`. Three existing
assertions had pinned the wrong answer (`r.mode = native`, `demo_mode != docker`, and the
`+2 more` label built on it) and were corrected rather than kept.

## Done when

- [x] the two conditions in Symptom render something that is true of an unset variable, with
      both asserted verbatim
- [x] a control: the same shapes with a default that does *not* satisfy the clause still read
      as a plain requirement, so the fix reads the field rather than always hedging
- [x] `WhenEquals` and `WhenIn` are both covered — this is one rule with two verdict types
- [x] the `+N more` case is asserted, not only the single-part case, since that is where the
      unreadable clauses hide
- [x] measured against ansible-core, not reasoned: a playbook where the named clause is
      satisfied by the default and the task genuinely runs
