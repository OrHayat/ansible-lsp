# T-213 — A defaulted membership test whose default is in the list is labelled runs only if

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | S    | T-121 | —          |

## Symptom

`item.state | d('present') in ['present', 'absent']` gets the inlay hint

> runs only if `item.state` is one of [present, absent]

The task **also runs when `item.state` is not set at all**, because the default `'present'` is
itself one of the listed values. "Runs only if" is then a false statement about the condition —
the exact wording a reader uses to decide whether they have to set the variable.

Live on ansible-core 2.21.2, `item: {}` in all three rows:

| condition | result |
| --- | --- |
| `item.state \| d("present") in ["present", "absent"]` | **ok (ran)** |
| `item.state \| d("present") in ["absent"]` | skipping |
| `item.state in ["present", "absent"]` *(control: no default)* | **fatal** — `item.state` is undefined |

The control matters: without the guard the same condition is a hard error, so the `default(...)`
is not decoration. It changes what happens on an unset variable, and the hint drops it.

## Cause

[`Verdict::WhenEquals`] carries `matches_default` and uses it to choose the wording:

```rust
(true, false) => format!("runs unless {var} changes from {value}"),
(false, false) => format!("runs only if {var} = {value}"),
```

[`Verdict::WhenIn`] has no such field. `strip_guards` reads the `default(...)` off the
membership test the same way it does for the equality test, and then throws the default value
away instead of asking whether it is in the list:

```rust
Verdict::WhenIn { var, values, negated }   // no matches_default
```

So `x | d('present') in ['present', 'absent']` and `x | d('present') in ['absent']` — which
behave *oppositely* on an unset `x` — produce verdicts that differ in nothing at all.

**T-188 made this reachable.** Before the tree, the `d(` spelling never classified (T-211), so
these conditions came out `Unknown` and the tool said nothing. Widening reach turned a silence
into a wrong answer, which is the trade this repo exists not to make.

## Blast radius

Three conditions across the eight pinned corpus trees, all in `debops`, all newly classified:

- `item.state | d('present') in ['present', 'absent']`
- `mount__enabled | bool and ... and item.state | d('directory') in ['directory', 'absent']`
- `mount__enabled | bool and item.state | d('mounted') in ['mounted', 'present', 'unmounted'] and ...`

Small, but every one of them is a wrong hint shipped to a user, and the shape is ordinary
enough that a bigger corpus will hold more.

## Fix

Give `WhenIn` a `matches_default` set the same way `WhenEquals` sets it — the default's
membership in the list, XOR `negated` — and branch the label on it. As shipped:

| `matches_default` | `negated` | label | unset, measured |
| --- | --- | --- | --- |
| `Some(true)`  | false | runs unless {var} leaves [{list}] | ran |
| `Some(true)`  | true  | runs unless {var} is one of [{list}] | ran |
| `Some(false)` | false | runs only if {var} is one of [{list}] | skipping |
| `Some(false)` | true  | runs only if {var} is not one of [{list}] | skipping |
| `None`        | false | runs only if {var} is one of [{list}] | fatal |
| `None`        | true  | runs only if {var} is not one of [{list}] | fatal |

Wording is a judgement call; the constraint is that only the rows measured as *ran* may say
"runs unless", because that phrase means runs-by-default here. `None` collapses onto the same
labels as `Some(false)` — neither runs when unset — but stays a distinct value because
[`invert`] has to treat them differently, which is the Outcome note below.

`requirement()` turned out **not** to be part of this. It drops `matches_default` for
`WhenEquals` too — `mode | default('a') == 'a'` renders `runs only if mode = a +1 more` — so
it is one pre-existing defect across both verdict types rather than a consequence of this one.
Filed as **T-215**; this ticket leaves `requirement()` exactly as it was.

## Done when

- [x] `WhenIn` records whether the default satisfies the membership, by the same rule
      `WhenEquals` uses
- [x] the four `(matches_default, negated)` combinations each have a label asserted, and no
      `matches_default: true` label begins "runs only if"
- [x] the three corpus conditions above are asserted verbatim, with the verdict each must
      produce — they are the measured cases, not invented ones
- [x] a control asserts the unguarded spelling (`x in ['a']`, no `default`) is unchanged, so
      the fix is about the guard and not about membership generally
- [x] `requirement()` is out of scope, and why is recorded in Fix — split to **T-215**
- [x] the live table in Symptom is re-run and still holds


## Outcome

`matches_default` on `WhenIn` is an `Option<bool>`, not a `bool`. `None` means there is no
guard, and unguarded an unset variable is a **fatal error rather than a skip** — so neither the
verdict nor its inverse runs by default. A plain `bool` would have made `invert` turn every
unguarded `x in [...]` into a claim that `not (x in [...])` runs by default, which is a new
wrong answer in place of the old one.

The unguarded negated label changed too, and that was not in the original plan: `x not in ['a']`
read `runs unless x is one of [a]`, and "runs unless" is this module's phrase for *runs by
default*, which an unguarded test never does. It now reads `runs only if x is not one of [a]`.

Corpus after the fix: **1697 classified, unchanged**. Reach is identical and only the framing
moved, which is what a fix to a wrong label should look like.
