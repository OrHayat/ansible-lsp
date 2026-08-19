# T-186 — A dotted path in a when: condition is labelled with its root variable, which says something false

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | bug  | P1       | S    | —          |

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

## Measured

Before, printed from `classify` directly — every arm that carries a variable, not only the one
the symptom was noticed on. `r` stands for a registered result throughout:

| condition                          | hint before                       | hint after                                   |
| ---------------------------------- | --------------------------------- | -------------------------------------------- |
| `not (r.skip \| default(false) \| bool)` | runs unless r is set        | runs unless r.skip is set                    |
| `r.enabled \| default(true) \| bool` | runs unless r is false          | runs unless r.enabled is false               |
| `r.flag \| default(false) \| bool` | runs only if r is set            | runs only if r.flag is set                   |
| `r.mode \| default('native') == 'native'` | runs unless r changes from native | runs unless r.mode changes from native |
| `r.mode in ['a', 'b']`             | runs only if r is one of [a, b]   | runs only if r.mode is one of [a, b]         |
| `r.stdout is defined`              | runs only if r is set             | runs only if r.stdout is set                 |
| `r.stdout \| length > 0`           | runs only if r is non-empty       | runs only if r.stdout is non-empty           |
| `r['stdout'] \| length > 0`        | *(none)*                          | runs only if r['stdout'] is non-empty        |
| `r.results[0].stdout \| length > 0` | runs only if r is non-empty      | runs only if r.results[0].stdout is non-empty |
| `hostvars[h].stdout \| length > 0` | *(none)*                          | *(none)* — refused, see below                |

All seven arms, so the ticket's guess that `RequiresNonEmpty` was not alone was right, and the
first test to go red was `UnlessSet` rather than the arm the symptom was reported on.

**This was live in the corpus, not only in a constructed case.** Of the 14 `REAL_WHENS` that
classified at all before the fix, **four** were rendering a claim about a root the condition
never mentioned:

- `ansible_facts.distribution in ['Ubuntu', 'Debian']` → *runs only if ansible_facts is one of
  [Ubuntu, Debian]*
- `ansible_facts.os_family in ['Ubuntu', 'Debian']` → the **same sentence**, from a different
  condition testing a different field. Two rows, one hint, and `ansible_facts` is always
  defined and is never one of those strings, so the hint was nonsense in both.
- `updates.results | length > 0` → *runs only if updates is non-empty*
- `volume_info_all.storage_volumes | length > 0` → *runs only if volume_info_all is non-empty*

The corpus classification count went **14/111 → 16/111**, measured both ways by running the
same probe against `HEAD` and against the fix. Nothing was lost: reading the accessor path
instead of cutting it off also reads the two `acme_*[N].subject_key_identifier is defined`
rows, which `plain_var` refused outright because the root it extracted contained a `[`. The
correctness fix bought reach rather than costing it, and `real_world_coverage_does_not_regress`
now pins 16.

## What was built

`VarRef { root, expr }` on every `var`-carrying arm. `Display` renders `expr`, so `label()` and
`requirement()` needed no edits — the format strings already interpolated `{var}` and now
interpolate the whole path. `Verdict::var()` returns `root()`, so the one thing a lookup can
use stayed a lookup-able name, which is what the ticket's "do not simply widen `var`" is about.

`plain_var` became `parse_var_ref`: a root identifier followed by any number of `.ident` and
literal `[...]` steps. The floor is the one this ticket proposed — a **non-literal subscript
refuses the whole reference**. `hostvars[h].x` has a perfectly good root (`hostvars`, not `h`),
but the path cannot be spelled without knowing `h`, and naming the root instead is the same
mistake in a smaller font.

`From<&str>` is `#[cfg(test)]` and panics on anything unparseable, so an expectation cannot
assert against a fabricated root and no production path can construct a `VarRef` that did not
come from the parser.

## Done when

- [x] `r.stdout | length > 0` names `r.stdout` on both display forms —
      `a_dotted_path_is_named_in_full_never_reduced_to_its_root` asserts `label()` **and**
      `requirement()` by exact string, so a fix that repaired only the one that was noticed
      fails
- [x] the plain case is the control in that same test: `hosts | length > 0` still equals
      `RequiresNonEmpty { var: "hosts" }` with its label unchanged, so "make everything
      Unknown" cannot pass
- [x] all seven `var`-carrying arms asserted against a dotted path, one row each in the same
      test — `UnlessSet`, `UnlessCleared`, `OnlyIfSet`, `WhenEquals`, `WhenIn`,
      `RequiresDefined`, `RequiresNonEmpty`. The extractor is shared, so the arm that surfaced
      the bug proves nothing about the rest (CLAUDE.md rule 3)
- [x] the deeper and bracketed paths are correct, not merely quiet:
      `accessor_paths_are_named_whole_or_refused` pins `r.results[0].stdout` and `r['stdout']`,
      and pins `hostvars[h].x` as `Unknown` — the one shape that is deliberately refused
- [x] the demo makes the claim too, and it is pinned:
      `demo/tasks/conditions.yml` gained the accessor row and a NO HINT row for the
      variable subscript, and `demo_exercises_every_problem_and_verdict` asserts the exact
      sentence *runs only if demo_result.stdout is non-empty*. A demo label is a claim
      (rule 4), and this one would otherwise rot silently
- [x] seen red before the fix, for the right reason — the three tests failed on
      `Some("runs unless r is set")` vs `Some("runs unless r.skip is set")`, on
      `r['stdout']` returning `None`, and on the demo sentence being absent

**Root cause:** see [[T-188]] — the classifier matches strings; this is one symptom.
