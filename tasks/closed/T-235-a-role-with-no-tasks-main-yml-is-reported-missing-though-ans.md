# T-235 — A role with no tasks/main.yml is reported missing, though Ansible loads it

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | M    | T-090 | —          |

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

## Outcome

**Status: `Skipped` with `RoleWithoutMainTasks`**, the variant the `tasks_from:` case already
used, now reached with or without `tasks_from:`. `Resolved` would have needed a target file,
and a defaults-only role has none. Every consumer reads the role directory back from the probed
paths through `Resolution::found_role_dir`.

The unmeasured question in Fix was answered: **the defaults did not reach the index.** The var
walk entered a role only through its task files, whose walk read the enclosing role's
`defaults/`/`vars/`. A role with no task file contributed nothing, so a play listing a role that
depends on `etcd_defaults` got `var-undefined` on every `etcd_*` default. `vars::follow_role`
now reads a found-but-taskless role's defaults, vars and dependencies directly, with the role on
the walk stack so two taskless roles that depend on each other terminate. Ansible fails that
cycle itself ("A recursion loop was detected"), so the test asserts only termination.

Measured on 2.21.2 while writing the tests: a role whose `tasks/` holds only `begin.yml`, listed
in `roles:`, loads its defaults (`begin_value=3`), and the `set_fact` in `begin.yml` does not
run (`begin_fact=UNSET`). The walk therefore does not enter a taskless role's other task files.

| consumer                           | before                 | after                                           | test |
| ---------------------------------- | ---------------------- | ----------------------------------------------- | ---- |
| `missing-file`                     | fires                  | silent; `truly_absent` still fires              | `a_role_with_no_tasks_main_is_found_by_every_consumer` |
| reference hover                    | none (Missing never hovers) | "runs no tasks here; its defaults, vars and dependencies still load" + role dir; no `tasks_from` claimed | same |
| go-to-definition on the role name  | silent                 | silent (no entry file; T-063's question)         | same (status + empty targets) |
| `ansible/references` paint         | unpainted              | unpainted (reads `Resolved` only)                | same |
| reverse index                      | no edge                | no edge (reads targets)                          | not asserted |
| `scan`                             | `MISSING FILES`        | `ROLES WITH NO tasks/main.yml`, heading reworded | manual run |
| var index, `var-undefined`         | false positive via dependency | defined; the control beside it still fires | same, plus `a_role_without_tasks_main_contributes_its_defaults_and_vars` |
| variable hover and go-to-definition | nothing to find       | land in `etcd_defaults/defaults/main.yml`        | same |
| `mutation.rs` (set_fact reach)     | no targets             | unchanged: a taskless role runs no `set_fact`    | not asserted |

Every new test was seen red: with the resolver back to `tasks_from`-only, with `follow_role`'s
taskless branch disabled, with the hover always taking the `tasks_from` sentence, and with the
cycle guard keyed per frame (stack overflow).

Corpus: `46dbdd3` is not in the local shallow kubespray clone. The run used `7198b21`, where
both roles have the same shape. The release binary from HEAD shows 4 `missing-file` across
`roles/etcd/meta/main.yml`, `roles/kubernetes/control-plane/meta/main.yml` and
`roles/network_plugin/calico/meta/main.yml`; the fixed binary shows 0 in those files.

Found alongside and filed as T-239: role defaults/vars in `main.yaml`, `main.json`, a bare
`main` or a `main/` directory never reach the index. The `*_from` findings went into T-063.

## Done when

- [x] `role_without_main_and_without_tasks_from_is_missing` asserts the measured answer —
      not missing — and the Status chosen above (renamed `…_is_found`)
- [x] a defaults-only role referenced from `roles:`, `dependencies:`, `import_role` and
      `include_role` draws no `missing-file`, one assertion each
- [x] a role name that exists nowhere is still `missing-file` — the control
- [x] every consumer listed in Fix is checked for this shape and asserted, including whether
      the role's defaults reach the variable index
- [x] the real binary run on kubespray `46dbdd3` shows 0 `missing-file` on `etcd_defaults` and
      `network_plugin/calico_defaults` (run on `7198b21`, see Outcome)
