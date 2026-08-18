# T-189 — Type-check a registered result's sub-keys against the module's RETURN schema

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P2       | M    | T-057      |

## Problem

## Approach

## Done when

- [ ]

## Problem

`RETURN` gives a type per key, and nothing reads it. The schema's strongest, least noisy use is
type checking, because a type is a fact about the key rather than a guess about runtime:

```yaml
stat:
  contains:
    exists: { type: bool }
    path:   { type: str }
```

Given `register: st`, these are provably wrong before the play runs:

- `st.stat.path.name` — attribute access on a `str`
- `st.stat.path[0]` — indexes a character, almost never the intent
- `st.stat.exists | length` — `length` of a `bool`
- `st.stat.path | int > 0` — string coerced, comparison meaningless

This is a different rule from the typo hint T-057 already scopes ("`loaded.ansible_factsss`
gets a soft hint"). That one asks *does this key exist*; this one asks *is this key being used
as its declared type*. The typo hint must stay soft because `RETURN` is not a complete list —
measured, `stat` returns 7 keys it never documents. A **type** claim does not have that
problem: when the key IS documented, its declared type is the module author's statement about
it, and using it otherwise is wrong regardless of what other keys exist.

## Approach

Needs T-057's parsed schema, hence the dependency — this is the consumer, not the parser.

Fire only where the type is declared and the misuse is unambiguous:

- attribute or index access on a scalar (`str`, `bool`, `int`, `float`)
- a filter whose input type is fixed and mismatched (`length` on a bool, arithmetic on a str
  without an explicit cast)

Deliberately do not fire on: `dict`/`list` (any access is plausible), `type: raw`, a key with
no `type:` at all, or any expression the parser cannot resolve (see [[T-188]] — this wants the
expression tree, not string matching, for the same reason).

Watch the engine-injected keys. T-057 already has a box for never flagging `failed`, `changed`,
`msg` and friends; they are absent from `RETURN` and typing them from the schema would be wrong.

## Done when

- [ ] each misuse above fires on a fixture with a documented scalar type, one assertion per case
- [ ] silent for `dict`/`list`/`raw`/untyped keys, asserted separately
- [ ] silent for engine-injected keys, and for a sub-key not in the schema at all — that is
      T-057's soft hint, not this rule, and the two must not double-report
- [ ] `# noqa` works, rule id matched exactly
- [ ] corpus gate: count the hits and read every one. A type error is provable, so a hit that
      turns out fine means the model is wrong, not that the rule is noisy — record which.
