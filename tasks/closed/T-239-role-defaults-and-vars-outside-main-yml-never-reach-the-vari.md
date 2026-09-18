# T-239 — Role defaults and vars outside main.yml never reach the variable index

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | M    | T-112 | —          |

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

## Outcome

`ScanCache::role_vars_files(subdir, name, bare_first)` ports `find_vars_files` for a role's
`defaults/` and `vars/`. It uses the extension order for the default `main`, and for an explicit
`*_from` when `bare_first` is set. For a directory hit it reuses `collect_vars_dir`, which
already ported `_get_dir_vars_files` for `group_vars/` (sorted, only extension-less subdirs
descended, hidden and `~` entries skipped). `vars::role_vars` reads whatever it returns. The
`*_from` half is T-063's; it calls the same function with `bare_first`.

No precedence change was needed for the directory form. `vars::effective` already breaks
same-precedence ties by file path, and path order is the sorted load order, so `b.yml` beats
`a.yml` and hover and go-to-definition follow.

Every new test was seen red: with `role_vars` back to the fixed `main.yml` path, with `.yaml`
tried before `.yml`, and with the directory branch disabled (and the `var-undefined` asserts
masked so that the hover and jump assertions were the ones to fail).

Corpus: kubespray `7198b21` uses both shapes in its central role. `kubespray_defaults` has
`defaults/main/` and `vars/main/` directories, `kubernetes/control-plane` has
`defaults/main/` and `vars/main.yaml`, and `kata_containers` has `defaults/main.yaml`.
`scan` goes from **96 `var-undefined` to 6**, with no new ones. The 6 left are a molecule
test file, a `scripts/` playbook and one `tests/` playbook, none of them in this shape.

Left alone: `include_vars::redundant_self_reload` matches only `<role>/vars/main.yml`, on
purpose ("stays in step with what we actually model"). It is now silent for a role whose
auto-loaded file is `vars/main.yaml`. That is a missed hint, not a false one.

## Done when

- [x] each row of the Symptom table has a test asserting the Ansible column, the `main.yml`
      row as the control — `role_defaults_and_vars_load_from_every_main_shape`
- [x] the `main/` directory form: every key defined, the later sorted file's value is the
      one hover and go-to-definition point at, and a nested subdirectory is read
- [x] `main.yml` and `main.yaml` side by side: only `main.yml`'s keys are defined
- [x] a role with neither `defaults/` nor `vars/` stays silent, not an error —
      `a_role_with_no_defaults_or_vars_dir_is_silent`
- [x] hover and go-to-definition on a use of a `defaults/main.yaml` key land on that file —
      one test per consumer, each seen red without the fix —
      `role_defaults_outside_main_yml_reach_every_consumer`
