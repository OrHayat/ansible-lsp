# T-220 — True, False and None are reported as undefined variables, because the literal list is lowercase only

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | bug  | P1       | S    | —          |

## Symptom

A `when:` using a capitalised Jinja literal gets a `var-undefined` WARNING on the literal:

```yaml
when: inventory_hostname == 'web01' or True
```

> `True` is never defined in any file reachable from this playbook — it may still come from
> inventory, facts, or extra-vars (-e).

Found in the editor on `demo/conditional_register.yml`, so it is the squiggle a user sees.
The lowercase spelling of the same word is silent, which is what makes it obviously wrong
rather than merely arguable — `true or True` warns once, on the second word.

Measured on jinja2 3.1.6 — `env.parse("{{ X }}")` and the type of the resulting node:

| spelling | node | ours before |
| -------- | ---- | ----------- |
| `true` `false` `none` | `Const` | silent — correct |
| `True` `False` `None` | `Const` | **reported undefined** |
| `null` `nil` `TRUE` | `Name` | reported — correct, they *are* variables |

## Cause

The same fact written down twice, and only one copy updated.

`jinja::parser::parse_primary` handles both spellings (`"true" | "True" => Const::Bool(true)`),
so the AST has always been right. But `condition::variable_uses` does not read the AST — it
scans words and filters them against `NOT_VARIABLES`, a hand-maintained list of Jinja filter
and test names that had `"true", "false", "none"` sitting in its "operators and tests" section.
Lowercase only. So the capitalised spellings fell through the filter and were collected as
variable references.

Two different kinds of fact were in one list: filter names, which are a judgement call about
what Ansible ships, and the literals, which the language fixes and jinja2's parser decides.

## Fix

The six literal spellings move to their own `LITERALS` const, checked alongside
`NOT_VARIABLES` at all three call sites. The doc comment records that the set is closed and
that `null`/`nil`/`TRUE` are deliberately *not* in it.

Seen red: dropping `"True"` and `"False"` from the new list fails the test with
`left: ["True", "False"], right: []`.

## Done when

- [x] `True`, `False` and `None` produce no `var-undefined`, asserted against jinja2 3.1.6's
      own `Const`/`Name` split for all six spellings
- [x] `null`, `nil` and `TRUE` are still reported — the control, since a fix that stopped
      reporting anything would pass the first half alone
- [x] the literals are no longer stored in the filter-and-test list they drifted out of
