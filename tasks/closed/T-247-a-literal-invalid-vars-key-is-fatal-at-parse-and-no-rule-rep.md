# T-247 — A literal invalid vars: key is fatal at parse and no rule reports it

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P2       | S    | T-106 | —          |

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

## Resolution

Measured on 2.21.3, then `keyword_values::vars_keys`, `role_entry_vars` and `set_fact_keys`:

- `vars:` on a play, block, task or `roles:` entry: a dash, non-ASCII, a number or a Jinja
  keyword fails at load. `class` passes, as for `register`. A null key fails with a different
  message and is left alone.
- **Where `--syntax-check` fails depends on the file.** In a playbook, or a task file reached by
  `import_tasks`, it fails. In a file reached by `include_tasks` it passes, and the include fails
  when it runs. T-108's `register` message claimed the first everywhere; both now say "this file
  fails to load wherever it is imported or included" in a task file.
- `set_fact`: fails when the task runs, skipped by a false `when:`. `cacheable` is not a fact.
  The `k=v` spelling is not split, as in `vars.rs`; that is T-046's.
- **`<<` is YAML's merge key.** The loader expands it, measured in `vars:`, `set_fact` and the
  `register` dict form, but our parser keeps it as a key. The first corpus run reported 24 of
  them in `set_fact: {<<: *anchor}`, all false; `keys()` now skips it for all three, including
  T-108's `register`, which had the same blind spot.

Corpus gate over 46 repos: 0 hits for the new checks. The 4 remaining are T-108's.

## Done when

- [x] a literal invalid `vars:` key on a play, block and task is an ERROR, each level measured
      — plus `roles:` entries; `vars_keys_are_checked_at_play_block_and_task`
- [x] `set_fact` keys are checked, with the message saying when it fails, measured —
      `set_fact_keys_fail_at_run_time`
- [x] a templated key still gets only `static-template`'s report — skipped here, asserted in
      the same test; reported by `static_fields::templated_vars_keys_are_errors_everywhere`
- [x] demo rows pinned by the existing keyword-values demo test
