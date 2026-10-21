# T-249 — A task's or block's vars: reach reads outside that task or block

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-112 | —          |

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

## Done when

- [ ] the two reads above are `var-undefined`, and the in-task and in-block reads stay clean
- [ ] hover and go-to-definition no longer name another task's `vars:`
- [ ] `rescue:`/`always:`, `include_tasks ... vars:` and handler `vars:` measured and pinned
- [ ] `dotted::absent_keys` stops skipping `TaskVars` and `BlockVars`
- [ ] corpus count: every new `var-undefined` hit read by hand
