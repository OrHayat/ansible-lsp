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

The field is misnamed, and that is the defect rather than a consequence of it. `r.stdout` is
not a variable — it is an **expression**: an accessor applied to the variable `r`. Jinja
resolves it as `getattr(r, 'stdout')` with an `r['stdout']` fallback. The same is true in every
language that has the syntax: `r.stdout` is a field access, `r` is the name being bound.

`Verdict::RequiresNonEmpty { var: String }` collapses those into one string, so the type cannot
represent the thing the condition actually talks about. Truncating to the root is the only
option the type leaves, and the wrong label follows from that automatically.

Model the two separately, because two different consumers want two different halves:

- the **root variable** (`r`) — what a definition lookup, hover target, or provenance walk must
  resolve; the accessor is meaningless to those
- the **full expression** (`r.stdout`) — what the label and `requirement()` must render, because
  it is what the condition is a statement about

Something like `{ root: String, expr: String }`, or a parsed accessor path if the shapes below
argue for it. Do not simply widen `var` to hold `"r.stdout"`: that makes `Verdict::var()` return
a value no lookup can use, and today's single caller (a test) would stop being a warning sign.

The shapes that must be representable, all real Jinja:

```
r.stdout              attribute
r['stdout']           subscript, same meaning
r.results[0].stdout   mixed, through a list
hostvars[h].x         subscript with a variable key — root is `hostvars`, not `h`
```

Deciding how far to model these is part of the ticket. A defensible floor: represent root plus
the literal accessor text, and classify anything with a non-literal subscript as `Unknown`
rather than guessing a root.

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

**Root cause:** see [[T-188]] — the classifier matches strings; this is one symptom.
