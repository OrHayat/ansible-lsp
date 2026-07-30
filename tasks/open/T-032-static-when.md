# T-032 — Static `when:` evaluation

| Status        | Priority | Size | Depends on |
| ------------- | -------- | ---- | ---------- |
| **partly done** | P2     | L    | —          |

## Shipped

`condition.rs` + inlay hints + 4 warning rules, 20 tests. Coverage measured by
`condition::corpus::when_coverage` (ignored by default):

```
tasks with when:      2669
  classified          1053 (39%)
  guarded by default  1051 (39%)
individual clauses    3468
  classified          1477 (43%)
problems found           0
```

39% of conditions now carry a verdict, up from 7% with the first four shapes. Verdicts:
`UnlessSet`, `UnlessCleared`, `OnlyIfSet`, `WhenEquals` (incl. `!=`), `WhenIn` (incl.
`not in`), `RequiresDefined`, `RequiresNonEmpty`, `Never`, `Unknown`.

Surfaced as **inlay hints**, not diagnostics — grey text to the right of the line, no
Problems row. Nothing here is wrong, so a diagnostic would have been 1053 rows of noise.

**Four warning rules also shipped** (`problems()`): `when-jinja-delimiters`,
`when-item-without-loop`, `when-assignment`, `when-unbalanced`. All four find **zero**
instances in `~/app/ansible` — the repo is clean — so they exist as insurance and are
demonstrated only in `demo/tasks/main.yml`, pinned by `demo_file_exercises_every_problem`.

### Bug worth remembering

The first version used `trim_end_matches(')')` to strip wrapping parens, which ate the
closing paren of a trailing `default(...)` and silently broke **every** guarded
comparison. Four tests caught it. Replaced with `strip_outer_parens`, which only strips a
balanced enclosing pair.

## Still open

- mutual exclusivity is computed (`Verdict::excludes`) but nothing consumes it — needs
  T-024's tree
- the remaining 61%; see the table below for why most of it is out of reach
- `WhenEquals` on an unguarded `x == 'lit'` stays `Unknown`; 253 clauses give that up

## Problem

`when:` is where the execution graph branches, and today it's opaque — captured as a boolean
`conditional` flag and nothing more. Two questions can't be answered:

- **what runs on a default invocation?** The thing you actually want from an execution tree. Not
  "here is everything that could run," but "here is what runs if you type
  `ansible-playbook site.yml` with no extra vars."
- **which branches are alternatives?** `mode == 'native'` and `mode == 'docker'` are mutually
  exclusive. A tree showing both as unconditional siblings is lying about control flow.

## What the corpus supports — and what it doesn't

3466 string conditions. Classified:

| Pattern                              | Count | %   | Statically evaluable? |
| ------------------------------------ | ----- | --- | --------------------- |
| other                                | 1962  | 56% | no                    |
| `X \| length`                         | 400   | 11% | no — needs the value  |
| `not (X \| default(false) \| bool)`  | 289   | 8%  | **yes, when unset**   |
| `X \| default('v') == 'lit'`         | 270   | 7%  | **yes, when unset**   |
| `X == 'lit'`                         | 253   | 7%  | only if X is known    |
| `X is (not) defined`                 | 201   | 5%  | partially             |
| `X \| default(true) \| bool`         | 119   | 3%  | **yes, when unset**   |

**Corpus-wide this is a losing game — 56% is unclassifiable.** A general Jinja evaluator is not
worth building, and one that guesses is worse than nothing.

**But scoped to `import_playbook`, the picture inverts.** Of its 56 conditions:

| Pattern                              | Count | %   |
| ------------------------------------ | ----- | --- |
| `not (X \| default(false) \| bool)`  | 45    | 80% |
| `X \| default('v') == 'lit'`         | 5     | 8%  |
| other                                | 4     | 7%  |
| `X is defined`                       | 2     | 3%  |
| `X \| length`                        | 2     | 3%  |

**93% of import-level conditions fall into four shapes.** That's where this is worth doing, and
it's exactly the level the execution tree branches at.

Only **2** conditions in the entire repo are literal booleans (`when: false`). So dead-code
detection is not the feature — there is no dead code to find. **Default-run evaluation is the
feature.**

## Approach

Not an evaluator. A **pattern matcher over a small closed set**, which is the only honest option
given that most conditions are unclassifiable. See *Shipped* above for the verdicts as built.

The `| default(D)` filter is what makes this tractable: it states the value when the variable is
unset, so *the condition carries its own default-run answer*. No variable resolution needed —
which matters, because Ansible has 22 variable precedence levels and resolving them statically is
not on the table.

**`Unknown` must be the fallback for anything not matched exactly.** The moment this guesses, the
execution tree becomes untrustworthy, and an untrustworthy tree is what T-011 was rejected for.

### Mutual exclusivity

Two imports whose conditions are `DependsOn { var: v, equals: a }` and
`DependsOn { var: v, equals: b }` with `a != b` cannot both run. Group them in the tree as
alternatives — one node with branches — rather than as siblings. That's the 5 `eq-on-default`
cases at import level, and 270 corpus-wide if it ever extends past imports.

### What this is *not*

- **not variable resolution.** `X == 'lit'` where X comes from inventory stays `Unknown`. 253
  cases give that up deliberately.
- **not correctness analysis.** It answers "what runs by default," never "is this right."

### Where it surfaces

T-024's TreeView is the consumer: nodes labelled *runs by default* / *skipped by default* /
*alternative branch* / *unknown*, with a toggle to hide default-skipped subtrees. Possibly hover
(T-029) too. Building it without a consumer is pointless, so land T-024 first or in parallel.

## Done when

- [x] the import-level shapes are recognised — 39% of all conditions, 93% of import-level
- [x] everything else is `Unknown`, measured by the corpus test so drift is caught
- [x] `skip_x | default(false)` and `default(true)` invert correctly
- [x] `when: false` yields `Never`
- [x] the pattern set lives in one closed table, limits readable at a glance
- [ ] mutual exclusivity is consumed by something (needs T-024)
- [ ] a toggle to hide default-skipped subtrees in the tree
