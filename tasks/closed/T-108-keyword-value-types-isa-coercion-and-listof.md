# T-108 — Keyword value types: isa coercion and listof

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P1       | M    | T-106 | T-107      |

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

- [x] a `listof` violation is an ERROR naming the expected item type — `keyword_values::listof`
      for `collections`, `gather_subset` and `listen`; `hosts` is `placement::hosts` and a
      container in `tags` is `placement::tags_checks` (T-110)
- [x] a play with no `hosts:`, or an empty/None entry, is an ERROR — the empty/None/shape
      half shipped in T-110 (`placement::hosts`); the missing key is
      `placement::missing_hosts`, demo row in `demo/placement.yml`
- [x] `notify:` containing a comma warns that it is one handler name, not two —
      `placement::comma_notify`, rule `notify-comma`, demo `demo/notify_comma.yml`
- [x] `register`, `loop_var` and `index_var` are checked against `validate_variable_name`
      — `keyword_values::variable_names`, rule `invalid-keyword-value`, demo rows in
      `demo/keyword_values.yml`
- [x] `vars_prompt` keys are checked against the closed set — shipped in T-110
      (`placement::vars_prompt`, `keywords::VARS_PROMPT_KEYS`); this box was never ticked

## Variable names — measured 2.21.3

The two kinds fail at different times, and the messages say which:

| Shape                                            | Ansible                                        |
| ------------------------------------------------ | ---------------------------------------------- |
| `register: my-var` / `none` / `café` / `42` / `""` | load error — `--syntax-check` fails            |
| `register: {a-b: x}` (projection form)           | load error on the key                          |
| `register: class` / `null` / `~`                 | runs                                           |
| `register: "{{ x }}"`                            | load error — reported by `static-template`, not here |
| `loop_var: my-item` / `True` / `null`, `index_var: my-idx` / `5` | task fails at run time; `--syntax-check` passes |
| the same under `when: false`, task or block      | **still fails**                                |
| with `ignore_errors` / `rescue`                  | caught like any task failure                   |
| with no `loop:` at all                           | still fails — `post_validate` runs regardless (T-155 measured valid names only) |
| `loop_var: "{{ v }}"`                            | rendered first, then checked                   |

Known silences, not false claims: a plain YAML 1.1 bool (`register: yes`) reads as the
string `yes` because the parser drops scalar style (T-162); `loop_var: null` is fatal but
skipped. A `register` projection value must be an expression over `_task.result` —
`_task.result.msg` works, a bare `"{{ msg }}"` fails at run time — which nothing checks yet.

Found on the way, filed as T-247: a **literal** bad `vars:` key (`vars: {my-var: 1}`) is
fatal at parse through the same `validate_variable_name` (`base.py:620`), and no rule
reports it — `static-template` covers only the templated key.

## Missing `hosts:` — measured 2.21.3

"The field 'hosts' is required but was not set." is a post-validate error, not a load error:
`--syntax-check` reports it, but a run executes every earlier play and dies on reaching this
one, `tags: [never]` or not. `hosts: []` by contrast fails at load, before any play runs.

Two deliberate silences. A file whose only play lacks `hosts:` reads exactly like a task file,
since `hosts:` is what makes a playbook; and an entry with a key no play accepts (a stray
top-level `- debug:`) is refused on that key first, which is the invalid-attribute rule's.

## `notify` and commas — measured 2.21.3

`notify: "restart a, restart b"` with handlers `restart a` and `restart b`: `--syntax-check`
passes, and the run fails with "The requested handler 'restart a, restart b' was not found"
once the task reports changed. The same string inside a list (`["a, b"]`) is the same one name.
A handler `name:` or a `listen:` topic equal to the whole string answers it, and the run works.

The rule is a WARNING, and fires only on proof of intent: every comma-split part is a handler
name or topic of the same play, and nothing in the play answers the whole string. Silent when
any handler in the play has a templated `name:` (it could render to the string, T-196), and
when a part names no handler here — handlers from `roles:` are not in the set, so that case is
a miss rather than a guess. The general "names no handler at all" case is T-196's.

## `listof` — measured 2.21.3

| Keyword         | Container or bare-dash item                                          |
| --------------- | -------------------------------------------------------------------- |
| `collections`   | load error, `Keyword 'collections' items must be of type 'str', not 'dict'.` |
| `listen`        | load error, same shape, `list` / `dict` / `NoneType`                  |
| `gather_subset` | `--syntax-check` fails, `Error processing keyword 'gather_subset': …`  |
| `tags`          | not this path: a container crashes earlier ("unhashable type"), a null item runs fine |
| `hosts`         | `_validate_hosts` runs first, with its own messages                  |

A lone mapping is wrapped into a one-item list, so it fails the same way; a null *value* is an
empty list and fine. Scalar style is dropped (T-162), so `gather_subset: [1]` reads as a string
and stays a miss, and so does an explicit `null` item, which reads the same as `"null"`.
