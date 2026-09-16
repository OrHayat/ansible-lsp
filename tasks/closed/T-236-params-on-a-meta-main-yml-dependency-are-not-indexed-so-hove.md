# T-236 — Params on a meta/main.yml dependency are not indexed, so hover and go-to-definition go silent on them

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P2       | S    | T-112 | —          |

## Problem

A param on a play's `roles:` entry is a variable definition we index. The same param on a
role's `meta/main.yml` `dependencies:` entry is not, although Ansible treats the two the same
way — both entries are `RoleInclude` objects (`metadata.py:59-62`), and T-164 measured that a
dependency's params reach the role as variables on 2.21.3.

Measured through the real `ansible-lsp` binary (`textDocument/hover` and
`textDocument/definition`, cursor on `app_env` inside the braces), same entry in two files:

```yaml
# play.yml
- hosts: all
  roles:
    - role: web
      app_env: staging
      app_dir: "/etc/{{ app_env }}"
```

```yaml
# roles/app/meta/main.yml
dependencies:
  - role: web
    app_env: staging
    app_dir: "/etc/{{ app_env }}"
```

| file                      | hover                             | definition   |
| ------------------------- | --------------------------------- | ------------ |
| `play.yml`                | "role param … = `staging`"        | `play.yml:4` |
| `roles/app/meta/main.yml` | `null`                            | `null`       |

Found while doing T-164, which added the `role-param-not-keyword` diagnostic for these
entries — so the diagnostic now sees them and the variable index still does not.

## Approach

`vars.rs` reads params only from `Play.roles` (`for param in &r.params`, around
`vars.rs:681`). T-164 added `ast::dependency_roles`, which builds the same `RoleUse` values
for a `meta/main.yml`, so the extraction exists; the definitions walk needs to read it for a
role's own meta file, with the same entry-scoped `scope` the `roles:` params carry (T-100's
scope rule — a param is visible within its entry and to the role, not elsewhere).

Enumerate the readers of the index before choosing where this goes (working rule 3): hover,
go-to-definition, the `ansible/references` variable paint, `path_substitution_hover`, and
`var-undefined`. Only hover and definition are measured above.

Out of scope, and unmeasured: whether a dependency's params should be visible from the
*dependency role's* files (`roles/web/tasks/main.yml` reading `app_dir`). The `roles:` side
has the same question; answer both together if it comes up.

## Done when

- [x] hover and go-to-definition on a param use inside a `dependencies:` entry find the param
      on the same entry, through the real handler —
      `a_dependency_param_hovers_and_jumps_within_its_own_entry`
- [x] the same name on a *different* dependency entry is not offered — the scope control —
      `a_dependency_param_is_not_offered_to_another_entry`
- [x] every other reader of the index listed above is checked and asserted for this shape —
      paint: `a_dependency_param_is_painted_only_within_its_own_entry`; `var-undefined` and
      `var-uncovered-when`: `a_dependency_param_draws_no_variable_diagnostic` (the undefined
      rule is playbooks-only, so this was silent before too); `path_substitution_hover`: see
      below. `inert-import-var` was not on the list and already asks `in_scope_at`; an
      `import_playbook` line is never inside a role entry
- [x] the `play.yml` half of the table above stays as it is —
      `a_play_roles_param_still_hovers_and_jumps`

Each new test was seen red first: with the meta read removed, with the role-name hole removed,
with the substitution hover's scope check removed, and with `EntryScope::covers` always true.

## The role name is not in its entry's scope

Indexing the dependency params exposed a wrong answer the `roles:` side already gave. The
entry scope covered the whole entry, **including the role name**, and ansible renders the name
to pick the role before the entry's params and `vars:` are bound. Measured on 2.21.3:

| case                                                              | result                    |
| ----------------------------------------------------------------- | ------------------------- |
| `role: "{{ flavor }}"` + `flavor: web`, on `roles:` and on `dependencies:` | `'flavor' is undefined` |
| the same with `flavor: web` written *first*                       | `'flavor' is undefined`   |
| the same with `vars: {flavor: web}` on the entry                  | `'flavor' is undefined`   |
| control: `flavor: web` in play `vars:`                            | loads `web`               |
| a play task's `include_tasks: "{{ app_env }}.yml"` after a `roles:` entry with `app_env` | `'app_env' is undefined` |

What the tool said, through the real handlers:

| position                      | reader     | `roles:` before | meta, first cut of this fix |
| ----------------------------- | ---------- | --------------- | --------------------------- |
| `role: "{{ flavor }}"`        | hover      | "`flavor` = `web`" | "`flavor` = `web`"       |
|                               | definition | silent          | jumped to `flavor: web`     |
|                               | paint      | coloured        | coloured                    |
| task include after `roles:`   | hover      | "`app_env` = `staging`" | —                   |

Two fixes, both shipped here since the first cut would otherwise have added the jump:
`VarDef::scope` is now an `EntryScope` (the entry, less the name span) so every reader of
`in_scope_at` gets the hole; and `path_substitution_hover`, the one reader that never asked
about scope, now does. Pinned by `a_role_name_does_not_read_its_own_entrys_params_or_vars`
(param, entry `vars:`, dependency), its control `a_role_name_still_reads_a_play_var`, and
`a_play_task_include_does_not_substitute_an_entry_param`.

The resolver's `known_literals` also ignores scope, and was measured not to matter: the
templated include above stays `Skipped` and unpainted.

### The warning on that line

The hole also made `var-undefined` fire on a role name that reads an entry variable, which is
right, but with the play-tasks text ("not the play's tasks"). Two more gaps on the same line,
measured on 2.21.3:

| entry                                                         | ansible                  | we said        |
| ------------------------------------------------------------- | ------------------------ | -------------- |
| `role: "{{ flavor }}"` + `when: false`                         | `'flavor' is undefined`  | —              |
| `role: "{{ flavor }}"` + `flavor: web` + `when: flavor is defined` | `'flavor' is undefined` | silent        |
| `- "{{ flavor }}"` after another entry's `flavor: web`         | `'flavor' is undefined`  | play-tasks text |
| control: `role: web` + `when: false`                           | skipping                 | —              |
| control: task `debug: "{{ flavor }}"` + `when: flavor is defined` | skipping              | silent         |

The entry's `when:` is checked on the role's tasks, after the name has loaded the role, so it
guards nothing about the name. `Site::role_name` now marks the use, the entry's `when:` is left
off its guard, and the warning names the role name. Pinned by
`a_role_name_reading_an_entry_variable_is_warned_about_as_a_role_name`,
`a_role_entrys_when_does_not_guard_its_role_name` and
`a_role_entrys_when_guards_its_params_and_not_its_name`.

Still wrong and not fixed here: inventory `group_vars`/`host_vars`/inline vars do not reach a
role name (measured, all four spellings fail, with a task-read control proving each inventory
loads), and we treat them as defining it — silent, and the hover shows the value.
