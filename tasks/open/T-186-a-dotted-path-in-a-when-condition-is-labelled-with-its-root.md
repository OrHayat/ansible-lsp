# T-186 — A dotted path in a when: condition is labelled with its root variable, which says something false

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P1       | S    | —          |

## Symptom

`condition::classify` keeps only the **root** of a dotted path, and the inlay hint then makes a
claim about the wrong thing. Measured:

| condition               | verdict                                | inlay hint shown             |
| ----------------------- | -------------------------------------- | ---------------------------- |
| `hosts \| length > 0`    | `RequiresNonEmpty { var: "hosts" }`    | `runs only if hosts is non-empty` |
| `r.stdout \| length > 0` | `RequiresNonEmpty { var: "r" }`        | **`runs only if r is non-empty`** |

The second row is not what the condition says. `r` is a registered result and is essentially
always non-empty — it is a dict with `changed`, `failed` and friends in it. What has to be
non-empty is `r.stdout`. So the hint states a requirement that is satisfied in cases where the
task does **not** run, which is the wrong direction: a reader trusting it concludes the task
will run when it will be skipped.

P1 by the board's test — the tool lies, in a hint the user did not ask for and cannot easily
check. `requirement()` carries the same error (`r non-empty`), so both display forms are wrong.

Reproduced without an editor:

```
classify("r.stdout | length > 0")
  -> RequiresNonEmpty { var: "r" }
  -> label()       = Some("runs only if r is non-empty")
  -> requirement() = Some("r non-empty")
```

## Cause

`plain_var` / `parse_defaulted` extract an identifier and stop at the dot, so the subscript or
attribute tail is dropped rather than making the shape unreadable. The classifier then reports
a match it does not really have.

`RequiresNonEmpty` is not necessarily alone here — every arm that carries a `var` is fed by the
same extractors (`UnlessSet`, `OnlyIfSet`, `WhenEquals`, `WhenIn`, `RequiresDefined`). Check
each before fixing only the one that was noticed. This is CLAUDE.md rule 3: the defect is in
the shared extractor, not in the arm that surfaced it.

## Fix

Two candidate directions; pick with a probe, do not assume:

1. **Carry the full path** so the label reads `runs only if r.stdout is non-empty`. Correct and
   keeps the hint. `Verdict::var()` then returns something that is not a variable name — today
   that is safe, because `var()` has exactly **one** caller and it is a test, but a future
   consumer doing a definition lookup on it would break. If this direction is taken, either
   split the field (root for lookup, full path for display) or document the meaning at the
   type.
2. **Refuse the shape** — a dotted path classifies as `Unknown`, so no hint is shown. Loses
   information but cannot mislead.

Direction 1 is preferred: the hint is genuinely useful on a registered result, and silence is
the outcome we already get for everything unclassified.

Whichever is chosen, `label()` and `requirement()` are the only display consumers
(`main.rs:1909`, `main.rs:1917`) and both must be asserted, not just the one in the repro.

## Done when

- [ ] `r.stdout | length > 0` either names `r.stdout` or produces no hint at all — never a
      claim about `r`, asserted on **both** `label()` and `requirement()`
- [ ] the plain case `hosts | length > 0` still classifies exactly as it does today, in the same
      test, so the fix cannot be "make everything Unknown"
- [ ] every other `var`-carrying arm checked against a dotted path, one assertion each, since
      they share the extractor
- [ ] a deeper path (`r.results[0].stdout`) is asserted too — either correct or `Unknown`,
      not a claim about `r`
- [ ] seen red before the fix
