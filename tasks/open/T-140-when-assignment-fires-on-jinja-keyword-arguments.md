# T-140 — when-assignment fires on Jinja keyword arguments

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P1       | S    | —          |

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

- [ ] the three cases in `condition::corpus::JINJA_KWARG_NOT_ASSIGNMENT` stop being reported
- [ ] `condition::corpus::a_jinja_keyword_argument_is_flagged_today_and_should_not_be` is
      inverted (it asserts today's wrong answer on purpose)
- [ ] `mode = 'docker'` is still reported — the rule must keep catching the real mistake
- [ ] `x == 'a=b'` still stays silent
