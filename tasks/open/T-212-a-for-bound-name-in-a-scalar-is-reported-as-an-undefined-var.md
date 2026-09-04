# T-212 — A `{% for %}`-bound name in a scalar is reported as an undefined variable

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-114 | T-040      |

## Symptom

A Jinja statement in an ordinary YAML scalar binds a name, and we call that name undefined.
Measured on this playbook:

```yaml
- hosts: all
  tasks:
    - debug:
        msg: "{% for h in groups.web %}{{ h }}:{{ port }} {% endfor %}"
```

`vars::undefined_uses` returns `["h", "port"]`, and `main.rs:1271` turns each into a
`var-undefined` WARNING reading *"`h` is never defined in any file reachable from this
playbook"*. `h` is bound by the `{% for %}` five characters to its left.

Both halves are wrong, and the second is the one that will not show up in a bug report:

| | reported | truth |
| --------------------------- | ------------- | -------------- |
| `vars::uses`                 | `h`, `port`   | `groups`, `port` |
| `jinja2.meta.find_undeclared_variables` | — | `groups`, `port` |

So we invent a use of `h` that does not exist *and* miss the use of `groups` that does. The
first is a false diagnostic; the second is silent under-reporting, which no user will notice.

**The control comes out different** — the same scalar in a play whose `vars:` really define
`h` and `port` returns `undefined: []`, so the sweep is capable of reporting nothing. And
`{% set q = 1 %}{{ q }}` is already clean, so a Jinja-statement exemption exists; it is
`{% set %}`-shaped and nothing else was added beside it.

## Cause

`vars.rs:273` is `while let Some(open) = text[i..].find("{{")`. Nothing in the crate ever looks
for `{%`. T-049 shipped the model deliberately — "a `when:` value is one expression, any other
scalar is literal text with `{{ }}` islands" — and that model has no place to put a binding.

The model is wrong about which surface this is. Ansible has two compile paths
(`_engine.py:293`): `when:`/`loop:`/`assert.that` go to `_compile_expression`, which cannot
contain a statement, and **everything else** goes to `_compile_template` → `env.from_string`,
where the whole document grammar is legal. A `msg:` is the second kind, exactly like a `.j2`
file is. We have been treating it as the first with `{{ }}` islands bolted on.

`{% set %}` being already exempt is the tell: the gap was noticed once, at one spelling, and
patched there. `for`, `with`, `macro` and a `{% set %}` written any other way all still bind
names we cannot see. That is T-188's thesis one level up — the next spelling breaks it again.

## Fix

Read the scalar as what Ansible reads it as: a template. That is T-040's deliverable — the five
lexer states and all 14 statement forms — applied to a YAML scalar instead of a `.j2` file, so
this is blocked on it rather than on inventing a second Jinja reader.

On top of the parsed document, the binding constructs and their scopes:

| tag | binds |
| --------- | ------------------------------------------------- |
| `for` | the loop target(s), plus `loop`, and `else` is outside the body |
| `set` | the target, for the rest of the block — both the inline and `{% set %}…{% endset %}` forms |
| `with` | its assignments, for its body only |
| `macro` | the macro name outside, its parameters plus `varargs`/`kwargs`/`caller` inside |
| `block` | `super`, and `scoped` changes what the body can see |
| `import`/`from` | the imported names |

`jinja2.meta.find_undeclared_variables` is the oracle: it already answers this question for a
whole template and is what the differential test should compare against, name-set for name-set,
over the `.j2` files and the templated scalars of the pinned corpus trees.

**A stopgap is available and is deliberately not taken here.** `{% for X in %}` could be
exempted at the string level the way `{% set %}` already is, which would kill the false warning
without the grammar. It would not fix the missing `groups`, it would not cover `with`, `macro`
or the block form of `set`, and it adds the fourteenth special case to the code T-188 exists to
delete. If the P1 needs to stop lying before T-040 lands, take the stopgap knowingly and say so
in the ticket — do not let it close this one.

## Done when

- [ ] `h` is not reported for the Symptom playbook, and `groups` is
- [ ] every binding tag in the table above is asserted, each with a control in which the same
      name really is undefined and *is* still reported
- [ ] scope is respected, not just the binding: a name bound in a `{% for %}` body is undefined
      after `{% endfor %}`, and `{% macro %}` parameters do not leak out of the macro
- [ ] name sets match `jinja2.meta.find_undeclared_variables` across the corpus trees' `.j2`
      files and templated scalars, in the T-184 gate shape
- [ ] the `{% set %}` exemption is replaced by the general rule, not left beside it
- [ ] T-114's standing box honoured: no rule in this epic fires on the corpus without a human
      confirming the hit is real

## Corpus, 2026-09-04

Measured on the reference tree at `186c7ed5` (768 files), with the generated inventory
present so the T-225 noise is out of the picture: 197 `var-undefined` hits, of which these
are this ticket's, each read by hand:

| shape | names | hits |
| ----- | ----- | ---- |
| `{% for X in … %}` in a block scalar or list-item string, then `{{ X.y }}` | `_ring`, `rpm`, `pool`, `dns`, `ip`, `scenario`, `host`, `vip`, `policy` | 12 |
| `loop.index` inside such a `{% for %}` body | `loop` | 2 |
| a keyword argument of a filter call, read as a name — `map(attribute='stdout')`, `int(base=16)` | `attribute`, `base` | 11 |
| Go template text inside `{% raw %}…{% endraw %}` | `end` | 1 |

The last two rows are not bindings, but they fall to the same fix: a real parse of the
scalar does not see a call's keyword as a name and does not tokenise inside `raw`. Neither
should get a spelling-specific patch beside the `{% set %}` one — that is the pattern the
Cause section names.
