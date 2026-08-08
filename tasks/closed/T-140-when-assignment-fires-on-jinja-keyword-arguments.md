# T-140 — when-assignment fires on Jinja keyword arguments

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | bug  | P1       | S    | —          |

## Symptom

We report `` single `=` is assignment, not comparison — Jinja raises a syntax error here at
runtime. Use `==`. `` on conditions that are valid and run. The advice is actively harmful:
following it turns `map(attribute='path')` into `map(attribute=='path')`, which really is a
syntax error.

Found by the T-077 corpus sweep over kubespray (Apache-2.0, 1011 files, 2261 `when:` clauses).
It reported 12 `when-assignment` problems; **all 12 were this**, none a real fault:

```yaml
when: crio_version is version("1.29.0", operator=">=")
when: force_etcd_cert_refresh or not item in etcdcert_master.files | map(attribute='path') | list
when: x | selectattr("path", "equalto", p) | map(attribute="checksum") | first | default('')
```

`attribute=` and `operator=` are Jinja keyword arguments. `selectattr`, `rejectattr`, `map`,
`groupby`, `version`, `sort` and `dict2items` all take them, so this fires across ordinary
filter usage rather than in some corner.

## Cause

`condition::problems` (`condition.rs:282-288`) strips string literals before looking for a
lone `=`:

```rust
let bare = strip_strings(cond);
if lone_equals(&bare) { out.push(Problem::AssignmentNotComparison); }
```

`strip_strings` (`condition.rs:535`) replaces every character of a quoted run — quotes
included — with a space, so `map(attribute='path')` becomes `map(attribute=      )`.
`lone_equals` (`condition.rs:561`) then looks only at the characters either side of each `=`:

```rust
if matches!(prev, Some(b'=' | b'!' | b'<' | b'>' | b'~')) || next == Some(b'=') { continue; }
return true;
```

For `attribute=` the previous byte is `e` and the next is a space, so it returns `true`. The
quoted value was the only thing distinguishing a kwarg from an assignment, and stripping
already threw it away.

Stripping is right for the other rules — it is what stops `x == 'a=b'` being read as an
assignment — so the fix is not to stop stripping.

## Fix

The discriminator is **position plus the preceding character**, both still available after
stripping:

| Text | `=` preceded by | inside `(` | verdict |
| --- | --- | --- | --- |
| `map(attribute='path')` | `e` (identifier) | yes | keyword argument |
| `version(x, operator='>=')` | `r` (identifier) | yes | keyword argument |
| `mode = 'docker'` | space | no | assignment — still report |
| `mode='docker'` | `e` (identifier) | no | assignment — still report |

So in `lone_equals`, skip an `=` that is *both* immediately preceded by an identifier
character (`[A-Za-z0-9_]`) *and* sits inside an unclosed `(`. Track depth with a running
counter over the already-stripped string, so parentheses inside string literals can't skew it.

Note `mode = 'docker'` survives on the space alone; the paren test is what keeps a
parenthesised assignment like `(a = b)` reported.

## Done when

- [x] the three cases in `condition::corpus::JINJA_KWARG_NOT_ASSIGNMENT` stop being reported

      And the four kubespray files they came from now yield 0 diagnostics through
      `references::extract` + `problems`, down from 6.

- [x] `condition::corpus::a_jinja_keyword_argument_is_flagged_today_and_should_not_be` is
      inverted — now `a_jinja_keyword_argument_is_not_an_assignment`
- [x] `mode = 'docker'` is still reported — the rule must keep catching the real mistake

      `assignment_is_not_comparison` grew a second loop pinning the shapes the narrowing must
      not have swallowed: `mode = 'docker'`, `mode='docker'`, `(a = b)`, `x and mode = 'y'`.
      The paren half of the test is what keeps `(a = b)` reported.

- [x] `x == 'a=b'` still stays silent

## Outcome

`lone_equals` now tracks a stack of open parens, recording for each whether a name ran into
it — `map(` is a call and takes keyword arguments, `(a or b)` is grouping and does not. An `=`
is skipped only when it is inside a call *and* preceded by a name character. Both conditions
are needed: `mode='docker'` has the name but no call, `(a = b)` has parens but no name.

`demo/tasks/conditions.yml` carries the GOOD case, so the fix is visible and not merely
asserted.

Found while fixing this, unrelated to it: the parser **panics** on a block scalar in a file
with no trailing newline. Filed separately.
