# T-223 — Undefined softening is a substring match, so a name containing 'defined' or a lookalike guard silences a real undefined read

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P2       | M    | T-112 | —          |

## Symptom

The undefined-variable rule goes silent on reads that ansible fails on, because "is this
read guarded" is decided by substring, not by the expression. Measured, ansible-core
2.21.2 against `undefined_uses` on a playbook that defines nothing:

| read                                                     | ansible | ours           |
| -------------------------------------------------------- | ------- | -------------- |
| `{{ user_defined_ports }} {{ predefined_routes }}`        | fatal   | silent         |
| `{{ foo }}` under `when: foo_bar is defined`              | fatal   | silent         |
| `{{ nope_missing \| default(also_missing) }}`             | fatal   | silent         |
| `{{ a_set \| default(nope_missing) }}`, `a_set` set later | ok      | silent         |
| `{{ foo }}` under `when: user_defined_x \| bool`          | fatal   | `foo`          |
| `{{ nope_missing }}` (control)                            | fatal   | `nope_missing` |

Rows 1–3 are silence where a diagnostic is due. Row 4 is the one that looks like a bug and
is not: on 2.21 an undefined name passed as `default()`'s argument does not raise when the
left side is defined — the lazy Marker model ([[T-116]]) — so the fix must not start
flagging it. Rows 5–6 are the controls that still fire.

Row 1 is how this was found. A probe for [[T-222]] used `definitely_undefined` as its
"must still flag" control, and the control stayed quiet; the name contains `defined`.

## Cause

Four sites decide "handled undefinedness" by text search, none by the Jinja AST that
`condition.rs` already parses with (`jinja::parse`, `ExprKind::Test`, `ExprKind::Filter`):

- `vars.rs:911` `softened()`: `expr.contains("default(") || expr.contains("defined")` —
  no ` is ` prefix, so any *name* containing `defined` matches (row 1), and any `default(`
  anywhere softens every name in the expression (row 3).
- `vars.rs:830` the guard check: `g.contains(&u.name) && g.contains("defined")` — `foo` is
  a substring of `foo_bar`, so `when: foo_bar is defined` guards `foo` (row 2).
- `condition.rs:648` `expression_swallows_undefined()`: ` is defined` / `default(`, whole
  expression, same row-3 shape.
- `condition.rs:835` `is_guarded()`: same test per clause, same shape.

The AST answers each of these exactly. `x is defined` is `Test { node: Name(x), name:
"defined" }` and guards `x` and only `x`; `x | default(...)` is `Filter { node, name:
"default" }` and guards its `node`'s root — and, per row 4, *also* its argument names, on
2.21. `a.b is defined` guards the root `a`. The whole-expression text check has no way to
say which name.

## Fix

One predicate on the Jinja AST — `guarded_names(expr) -> HashSet<String>` or a per-use
`is_guarded(expr, name)` — that walks the tree and collects the roots under `Test
{ name: "defined" }` (and the `Unary(Not)` wrapping of `is not defined`), and both the
node root and the argument roots of `Filter { name: "default" }`. Every one of the four
sites reads it. `softened()` and the `guard` substring go away; `expression_swallows_
undefined()` and `is_guarded()` keep their signatures and change their bodies.

Rule 3: the predicate lives once, and each consumer gets its own test row from the table
above. The `condition.rs` pair feed the condition hints and [[T-060]]'s guarded-typo
verdict, not only the undefined rule — enumerate those consumers before deciding the
predicate's shape, and measure each with the row-5 control.

Row 4 is the case to get right and the one a naive AST walk gets wrong: `default(y)` with
`y` undefined is *fine* when the left is defined. Guarding `y` on the strength of the
`default` it sits in is the correct 2.21 answer, and it is version-dependent — pre-2.19
would raise. Record which core the assertion holds on.

Unparseable expressions (the Jinja parser rejects) fall back to silence, as today: a
spurious match costs a missed report, never a false error.

## Landed

`condition::guards` walks the Jinja AST once and records, per name position, whether the
read is `Handled` (tested, defaulted, or on a branch an enclosing test keeps from
running) or evaluated `WhenUndefined(root)` (a `default` argument). `guard_at` answers
for one position; `positively_defined` answers for a `when:` clause. The four sites read
those: `handled_in_expression` and the guard-clause check in `vars.rs`,
`expression_swallows_undefined` and `is_guarded` in `condition.rs`. `VarUse` carries the
span of its expression so the rule can hand the right text over.

Row 4 is resolved per name at the use: a `default` argument is flagged only when the
defaulted root has no definition in scope — which is exactly when ansible evaluates it.

The corpus pin moved from 10 to 9 guarded clauses: `terraform_version_installed is not
defined or terraform_version_installed != terraform_version` has a bare `terraform_version`
that raises, and the substring had counted it.

## Done when

- [x] rows 1–3 of the Symptom table flag exactly the names shown as `fatal`, in `vars.rs`
      tests next to `undef`, with row 6 as the control in the same test
      (`softening_follows_the_expression_not_the_spelling`)
- [x] row 4 stays silent, with a comment naming the 2.21.2 run that says why
- [x] `when: foo_bar is defined` guards `foo_bar` and `foo_bar.x`, and not `foo` — one test
      each way round
- [x] `expression_swallows_undefined` and `is_guarded` answer from the same predicate, and
      each has a test where the substring answer and the AST answer differ
- [x] no `contains("defined")` or `contains("default(")` remains in `vars.rs` or
      `condition.rs`
