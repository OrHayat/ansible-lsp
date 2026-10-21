# T-249 — A task's or block's vars: reach reads outside that task or block

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | M    | T-112 | —          |

## Symptom

Measured on 2.21.3:

```yaml
- hosts: localhost
  tasks:
    - debug: {msg: "{{ tv.a }}"}
      vars: {tv: {a: 1}}          # prints 1
    - debug: {msg: "{{ tv.a }}"}  # 'tv' is undefined
    - block:
        - debug: {msg: "{{ bv }}"}
      vars: {bv: in-block}        # prints in-block
    - debug: {msg: "{{ bv }}"}    # 'bv' is undefined
```

`scan` reports neither undefined read. The variable index treats a `TaskVars` or `BlockVars`
definition as reaching reads in other tasks of the file, so `var-undefined` is silent on two
reads that fail, and hover can name another task's `vars:` as the definition.

Found while building T-221's missing-key rule: the read in a task with no `vars:` was matched to
a later task's `vars:` mapping, and so reported as "has no key" where the real failure is that
the name is undefined.

## Fix

Give `TaskVars` and `BlockVars` definitions a scope, the task's or block's span, and check it in
`Located::in_scope_at`, the one place `reaches` and `in_effect_for` both read (rule 3). Then
assert each consumer: `var-undefined`, hover, go-to-definition, and T-221's `undefined-key`,
which skips both sources until this lands (`dotted::absent_keys`).

Measure before claiming: a `block:`'s vars inside its `rescue:` and `always:`, an
`include_tasks` with `vars:` (the included file's tasks), and a handler's `vars:`.

## Resolution

`TaskVars` and `BlockVars` definitions now carry the task's or block's span as their scope
(`VarIndex::push_within`), checked by `Located::in_scope_at`, the one place every consumer reads.
Measured on 2.21.3 first: a block's `vars:` reach its `rescue:` and `always:`, a handler's reach
that handler, and an `include_tasks`'s `vars:` reach the included file but not the next task.
The first three are inside the span. The included file is another file, which a same-file scope
does not touch. The index never carried plain include `vars:` into the included file anyway
(only `apply:` vars travel on the edge, T-167), so that behaviour is unchanged rather than pinned.

`var-undefined`'s "defined, but out of scope" message assumed a `roles:` entry. It now names a
task's or a block's `vars:` when that is the source.

Corpus: no new hit on any of the 46 repos or the private corpus. The measured playbook above
gives both hits in the same `scan`, so the count could have moved.

## Done when

- [x] the two reads above are `var-undefined`, and the in-task and in-block reads stay clean —
      `task_and_block_vars_reach_only_their_own_task_or_block`
- [x] hover and go-to-definition no longer name another task's `vars:` —
      `a_task_or_block_vars_name_is_out_of_scope_after_it_in_every_consumer`, with the message
- [x] `rescue:`/`always:`, `include_tasks ... vars:` and handler `vars:` measured and pinned — the
      first and last pinned in the core test; the include case is cross-file, see Resolution
- [x] `dotted::absent_keys` stops skipping `TaskVars` and `BlockVars` — a read inside its own
      task's `vars:` now fires, in `a_missing_key_is_reported_only_where_the_read_fails_for_certain`
- [x] corpus count: every new `var-undefined` hit read by hand — there are none
