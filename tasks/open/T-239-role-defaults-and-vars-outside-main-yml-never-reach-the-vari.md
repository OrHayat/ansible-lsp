# T-239 — Role defaults and vars outside main.yml never reach the variable index

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-112 | —          |

## Symptom

A role whose defaults or vars live anywhere but `defaults/main.yml` / `vars/main.yml` gets a
false `var-undefined` on every use of them. Measured with `scan` on a fixture whose roles are
applied from a play's `roles:` and whose variables are used unguarded in the play's tasks:

| role file                       | Ansible 2.21.2                  | `scan`            |
| ------------------------------- | ------------------------------- | ----------------- |
| `defaults/main.yml` (control)   | defined                         | defined           |
| `defaults/main.yaml`            | defined                         | **undefined**     |
| `defaults/main.json`            | defined                         | **undefined**     |
| `defaults/main` (no extension)  | defined                         | **undefined**     |
| `defaults/main/a.yml`, `b.yml`, `sub/c.yml` | all keys defined; `b` wins over `a`; `sub/` is read | **undefined** |
| `vars/main.yaml`                | defined                         | **undefined**     |
| `vars/main/x.yml`               | defined                         | **undefined**     |

With both `defaults/main.yml` and `defaults/main.yaml` present, Ansible reads only
`main.yml` (`both_v=yml`), which is the first-hit-wins order below.

## Cause

`vars.rs:1506` reads two fixed paths — `role.join("defaults").join("main.yml")` and the same
for `vars/` — with the comment "fixed locations, no search". Ansible searches.

`Role._load_role_yaml('defaults'|'vars', main=None, allow_dir=True)` in
`playbook/role/__init__.py`, through `DataLoader.find_vars_files`:

- tries `main.yml`, `main.yaml`, `main.json`, then bare `main` — the first that exists wins;
- if that hit is a **directory**, loads every file under it recursively in sorted order,
  skipping hidden files and `~` backups (read, not measured), and merges them (later wins);
- a missing `defaults/` or `vars/` dir, or no match, is `None` — silent.

The task-file side already has this search (T-091 ported the extension order for
`tasks/main`). Only the var-file side still hard-codes `.yml`.

## Fix

Port `find_vars_files` once, as the lookup both role var subdirs go through, and have
`vars.rs` read whatever it returns. The directory form means one role subdir can yield
several `Located` definitions of one name — the sorted order must decide which is live, the
same as `combine_vars` does.

T-063 needs the same function for `vars_from:` / `defaults_from:` — with the bare name tried
**first** there — so build it with the entry name and that order as parameters, not
hard-coded to `main`.

Consumers to check for this shape (working rule 3): `var-undefined`, hover and
go-to-definition on a use (they should land in the file that won, not `main.yml`), the
role-scoped reads through `meta/main.yml` dependencies (`vars.rs:1513`), and the
`scan` undefined block.

## Done when

- [ ] each row of the Symptom table has a test asserting the Ansible column, the `main.yml`
      row as the control
- [ ] the `main/` directory form: every key defined, the later sorted file's value is the
      one hover and go-to-definition point at, and a nested subdirectory is read
- [ ] `main.yml` and `main.yaml` side by side: only `main.yml`'s keys are defined
- [ ] a role with neither `defaults/` nor `vars/` stays silent, not an error
- [ ] hover and go-to-definition on a use of a `defaults/main.yaml` key land on that file —
      one test per consumer, each seen red without the fix
