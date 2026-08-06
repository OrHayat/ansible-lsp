# T-108 — Keyword value types: isa coercion and listof

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P1       | M    | T-106 | T-107      |

## Problem

Every field attribute declares an `isa`, and `_get_validated_value`
(`playbook/base.py:457-509`) coerces to it or fails. A `listof` violation is fatal with
`Keyword %r items must be of type ...` (`:483-497`).

The cases that reach real repos:

- `hosts` is `required=True`, `listof=(str,)`, and `_validate_hosts` (`play.py:119-134`)
  additionally rejects an empty list, a `None` entry and a non-str entry. A play with no
  `hosts:` fails post-validate with `The field 'hosts' is required but was not set.`
  (`base.py:559-563`).
- `tags` accepts a comma-separated **string** and splits it (`taggable.py:53-54`). `notify`
  does **not** — it is a plain `isa='list'`, so `notify: "restart a, restart b"` is one
  handler named `"restart a, restart b"`. Two adjacent keywords, opposite behaviour, no
  diagnostic either way.
- `vars_prompt` entries accept only `{name, prompt, default, private, confirm, encrypt,
  salt_size, salt, unsafe}` with `name` required — fatal otherwise (`play.py:241-247`).
- `loop_control` must be a mapping and cannot itself be a variable (`task.py:346-354`);
  `loop_var`/`index_var` must be valid identifiers (`loop_control.py:45-57`); `register` must
  be a valid variable name (`task.py:363-375`).

Identifier validity is its own rule: `validate_variable_name` (`utils/vars.py:271-288`)
requires `.isidentifier()` and `.isascii()` and rejects the Jinja keywords `true false none
True False None not` (`_jinja_bits.py:83-95`). Python keywords like `class` **pass**, unlike
the older check it replaced.

## Approach

Types come with the keyword tables from T-107 — one column, not a second transcription. The
`notify` comma trap is worth its own message rather than a generic type error, since the
value is *valid*, just not what the author meant.

## Done when

- [ ] a `listof` violation is an ERROR naming the expected item type
- [ ] a play with no `hosts:`, or an empty/None entry, is an ERROR
- [ ] `notify:` containing a comma warns that it is one handler name, not two
- [ ] `register`, `loop_var` and `index_var` are checked against `validate_variable_name`
- [ ] `vars_prompt` keys are checked against the closed set
