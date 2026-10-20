# T-247 — A literal invalid vars: key is fatal at parse and no rule reports it

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | S    | T-106 | —          |

## Problem

`vars: {my-var: 1}` on a play fails `--syntax-check` with `Invalid variable name 'my-var'.`,
measured on 2.21.3. `_load_vars` (`base.py:612-628`) runs `validate_variable_name` on every key
of a `vars:` mapping. `static-template` reports a *templated* key, since braces are never
rendered there, but a literal invalid key — a dash, a Jinja keyword like `none`, non-ASCII — is
silent today.

Found while doing T-108's `register` / `loop_var` names, which use the same predicate.

## Approach

`keyword_values::is_variable_name` already is `validate_variable_name`, so this is placement,
not a new predicate. The same call also guards `set_fact` / `set_stats` keys
(`plugins/action/set_fact.py:46`) and inventory host and group vars (`inventory/host.py:123`,
`inventory/group.py:212`). Measure each of those before claiming it, since they fail at
different times: an action plugin fails when its task runs, not at load.

Leave templated keys to `static-template`, for the same reason T-108 did: one claim per token.

## Done when

- [ ] a literal invalid `vars:` key on a play, block and task is an ERROR, each level measured
- [ ] `set_fact` keys are checked, with the message saying when it fails, measured
- [ ] a templated key still gets only `static-template`'s report
- [ ] demo rows pinned by the existing keyword-values demo test
