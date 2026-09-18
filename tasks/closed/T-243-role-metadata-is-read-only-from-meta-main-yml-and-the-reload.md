# T-243 — Role metadata is read only from meta/main.yml, and the reload hint knows only vars/main.yml

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | M    | T-090 | —          |

## Symptom

Found while closing T-239, which fixed the same assumption for `defaults/` and `vars/`.

- **Dependencies in another spelling are dropped.** A role whose metadata is `meta/main.yaml`,
  `meta/main.json` or a bare `meta/main` has its `dependencies:` ignored by the var walk, so
  their defaults never reach the play and a use of one is a false `var-undefined`. The
  hover path and `scan` also extracted dependency references only from a file ending in
  `meta/main.yml`.
- **A shadowed file is checked as if Ansible read it.** `is_role_metadata` accepted any
  `meta/main.yml` or `meta/main.yaml`. With both present Ansible reads only `main.yml`, yet
  the `.yaml` file drew `invalid-attribute` on its keys and `missing-file` on its
  dependencies.
- **The reload hint is silent for other spellings.** `redundant-role-vars-include` matched
  only `<role>/vars/main.yml`, so re-loading an auto-loaded `vars/main.yaml`, or a file in
  `vars/main/`, got no hint.

Measured on 2.21.2, `roles: [r]` with `dependencies: [dflt]` in the named file:

| `meta/` contents                                   | dependency ran |
| -------------------------------------------------- | -------------- |
| `main.yml`                                         | yes            |
| `main.yaml`                                        | yes            |
| `main.json`                                        | yes            |
| `main` (no extension)                              | yes            |
| `main.yml/` as a directory, beside `main.yaml`     | yes, from `main.yaml` |
| `main/x.yml` only                                  | **no**         |
| `main.yml` with `dependencies: []` beside `main.yaml` | **no**      |

And `include_vars: main.yaml` inside a role whose `vars/main.yaml` is its entry point re-reads
that file (`ansible_included_var_files` names it).

## Cause

`Role._load_role_yaml('meta')` runs with `main=None` and no `allow_dir`. It tries `main.yml`,
`main.yaml`, `main.json`, then bare `main`, and the first *file* wins. A directory of one of
those names is passed over for the next spelling (`find_vars_files` `continue`s when
`allow_dir` is false). We hard-coded the name in four places: `vars::role_vars`,
`FileContext::is_role_metadata` (by name, no filesystem), `main.rs` `hover_at`, and `scan`.
`resolve::collections_in_scope` already probed in Ansible's order and needed no change.

## Fix

`workspace::role_meta_file(role, fs)` is the lookup. `is_role_metadata(path, fs)` is true
only for the file it returns, and every reader goes through that predicate (rule 3).
`include_vars::redundant_self_reload` takes a `ScanCache` and matches any file
`ScanCache::role_vars_files(vars, "main")` returns. Its message names the file it actually
matched.

`only_a_role_s_own_meta_main_is_validated_as_role_metadata` had been asserting the shadowed
case by accident: it wrote `meta/main.yaml` while `meta/main.yml` was still on disk and expected
a diagnostic. The setup now removes `main.yml` first, and the shadowed case is asserted
explicitly as silent.

Every new test was seen red: with `is_role_metadata` back to its name check, with
`role_meta_file` trying only `main.yml`, with the hover path back to `ends_with`, and with the
hint back to `vars/main.yml` equality. Kubespray `7198b21` gives the same findings before and
after; only the read count changed (954 → 938).

## Done when

- [x] every row of the table has an assertion — `role_meta_file_follows_ansibles_lookup`
- [x] dependencies in `main.yaml`, `main.json` and bare `main` reach the var index, and a
      shadowed `main.yaml`'s do not — `dependencies_load_from_every_meta_main_spelling`
- [x] `missing-file` and the reference hover answer in a lone `meta/main.yaml` and not in a
      shadowed one — `dependencies_in_meta_main_yaml_reach_the_reference_consumers`
- [x] a shadowed `meta/main.yaml` draws no `invalid-attribute` —
      `only_a_role_s_own_meta_main_is_validated_as_role_metadata`
- [x] the reload hint follows the auto-loaded file: a lone `main.yaml`, a `main/` member,
      not a shadowed `main.yaml` — `the_hint_follows_the_file_the_role_actually_auto_loads`
