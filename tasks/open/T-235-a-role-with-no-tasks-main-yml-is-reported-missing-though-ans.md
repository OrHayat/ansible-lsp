# T-235 — A role with no tasks/main.yml is reported missing, though Ansible loads it

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-090 | —          |

## Symptom

A reference to a role that exists but has no `tasks/main.yml` gets a `missing-file` warning:

```
no file found for `etcd_defaults`. Tried:
  roles/etcd_defaults/tasks/main.yml
  roles/etcd_defaults/tasks/main.yaml
  roles/etcd_defaults/tasks/main.json
  roles/etcd_defaults/tasks/main
```

Found through the real `ansible-lsp` binary on kubespray `46dbdd3` while verifying T-164: 3
warnings in `roles/etcd/meta/main.yml` and `roles/kubernetes/control-plane/meta/main.yml`, on
`etcd_defaults` (only `defaults/` and `vars/`) and `network_plugin/calico_defaults` (only
`defaults/`). Both roles exist, and both exist *to* carry no tasks — they hold defaults.

Ansible loads this shape everywhere. Measured on 2.21.3, fixture role `dflt` with only
`defaults/main.yml`:

| reference                          | result                                                        |
| ---------------------------------- | ------------------------------------------------------------- |
| `meta/main.yml` `dependencies:`    | runs clean; the depending role sees `dflt_value`              |
| a play's `roles:`                  | runs clean; the play's tasks see `dflt_value`                 |
| `import_role: {name: dflt}`        | runs clean; the next task sees `dflt_value`                   |
| `include_role: {name: dflt}`       | runs clean; `dflt_value` is `UNSET` after it (not public — expected) |

A role with a `tasks/` dir holding only `begin.yml` behaves the same under `include_role`,
`import_role` and `roles:` without `tasks_from:` — no error, no warning, nothing runs. The
control that must come out different: `roles: [truly_absent]` fails at load with
"The role 'truly_absent' was not found in: …".

So the warning is false in every position without `tasks_from:`. The `tasks_from:` case was
already right (T-218).

## Cause

`resolve.rs`, the `ReferenceKind::Role` arm: once `role_dir` finds the directory, the
resolution is the probe for `tasks/main.*`. With `tasks_from:` a failed probe becomes
`Skipped(RoleWithoutMainTasks)`; without it the probe's `Missing` stands, and `missing-file`
reports it.

It is also pinned the wrong way round. `role_without_main_and_without_tasks_from_is_missing`
asserts `Status::Missing`, with the comment "now main.yml really is required, so it's an
error" — the measurement above says it is not. Working rule 7: that assertion is a claim that
the false warning is right.

## Fix

The role directory existing is what decides found vs missing — `role_dir` returning `None` is
the real missing case, and it is reported separately already. Without `tasks_from:`, a role
with no `tasks/main.*` is found and simply contributes no tasks.

Which `Status` that is — `Resolved` to the role directory, or `Skipped` with
`RoleWithoutMainTasks` as the `tasks_from:` case does — is the decision to make first, by
enumerating the consumers of a role resolution rather than picking for the diagnostic alone
(working rule 3): the `missing-file` diagnostic, the reference hover (T-218's sentence),
go-to-definition on the role name, the `ansible/references` paint, the reverse index, and
`scan`'s `UNRESOLVED ROLE NAMES` block.

Not measured yet, and worth checking before choosing: whether the variable index reads
`defaults/` and `vars/` of a role whose reference resolves `Missing`. If it does not, uses of
`etcd_defaults`' variables in kubespray may also draw false `var-undefined` — the same bug
seen from a second consumer.

## Done when

- [ ] `role_without_main_and_without_tasks_from_is_missing` asserts the measured answer —
      not missing — and the Status chosen above
- [ ] a defaults-only role referenced from `roles:`, `dependencies:`, `import_role` and
      `include_role` draws no `missing-file`, one assertion each
- [ ] a role name that exists nowhere is still `missing-file` — the control
- [ ] every consumer listed in Fix is checked for this shape and asserted, including whether
      the role's defaults reach the variable index
- [ ] the real binary run on kubespray `46dbdd3` shows 0 `missing-file` on `etcd_defaults` and
      `network_plugin/calico_defaults`
