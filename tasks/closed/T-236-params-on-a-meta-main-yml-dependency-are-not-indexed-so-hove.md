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

### What exists when a role name is rendered

Only the play's `vars:` and `vars_files:`. The name is rendered when the play loads, before any
host or task. Inventory is read earlier than that, measured with an inventory script that left a
marker even though the play died at load, but its variables are stored per host, and no host has
been picked yet. Measured on 2.21.3 with the name on a play's `roles:` entry. Every row has a
control playbook that reads the same variable from a task and prints `flavor=web`:

| source                                                                  | role name  |
| ----------------------------------------------------------------------- | ---------- |
| play `vars:`, `vars_files:`                                             | runs       |
| `group_vars/all`, `group_vars/<group>`, beside the playbook or the inventory | undefined |
| `host_vars/<host>`, inline host var, `[all:vars]`                       | undefined  |
| `set_fact`, `register`, `include_vars`, `add_host` in `pre_tasks`       | undefined  |
| block `vars:`, task `vars:` in `pre_tasks`                              | undefined  |
| an earlier role's defaults, its vars                                    | undefined  |

Before, all twelve undefined rows were silent, the paint coloured the name, and ten hovers
showed the value. `VarSource::reaches_role_name` now holds the table, `Located::reaches` applies
it, and `path_substitution_hover` judges candidates by `reaches` on the use rather than by
position. The warning names the source. Pinned by `a_role_name_reads_only_play_vars_and_vars_files`
(all twelve, warning + hover + paint) and its controls
`play_vars_and_vars_files_still_reach_a_role_name_and_inventory_still_reaches_a_task`.

Corpus gate `role_name_reach_corpus`: 0 hits, because no tree holds a templated play `roles:`
name (the one `- role: '{{ … }}'` is data in a defaults file). The same sweep over the fixtures
above reports 12 of 12.

### `import_role`, `include_role`, and dependency names

The other three places a role is named, measured on 2.21.3. Each "undefined" row has a
control: for `import_role` it is the same fixture with `include_role`, which ran; for a
dependency it is the role's own task, which printed the value.

| source                                                          | `import_role` | `include_role` | dependency |
| --------------------------------------------------------------- | ------------- | -------------- | ---------- |
| play `vars:`, `vars_files:`                                      | runs          | runs           | runs       |
| block `vars:`, task `vars:`                                      | runs          | runs           | —          |
| the play's role defaults / vars (even from `pre_tasks`)          | runs          | runs           | —          |
| its own role's defaults / vars                                   | —             | —              | undefined  |
| inventory (`group_vars`, `host_vars`, `[all:vars]`)              | undefined     | runs           | undefined  |
| `set_fact`, `register`, `include_vars`, `add_host`               | undefined     | runs           | undefined (`set_fact`) |
| `when: flavor is defined` on the task, or on a block around it   | undefined     | skipped        | —          |

A dependency's column is the same whether its role is reached through `roles:`, `import_role`
or `include_role`. `include_role`'s name is an ordinary task argument and needed nothing.
`Site::load_name` is now a `LoadName` (a role entry or dependency, or `import_role`), each with
its own row in `VarSource::reaches_load_name`, and no `when:` is kept on such a use. No warning
is raised in a `meta/main.yml`, since its callers' plays are out of sight. The hover and paint
there stopped showing a role default, a role var or inventory as the name's value.

Pinned by `an_import_role_name_cannot_read_host_or_task_set_variables`,
`an_import_role_name_reads_play_block_task_and_role_variables`,
`no_when_guards_an_import_role_name_and_both_guard_an_include_role_name`,
`an_include_role_name_reads_every_source`,
`a_dependency_name_does_not_read_its_roles_variables_or_inventory`, and
`load_time_role_names_are_marked_by_form_and_guarded_by_nothing`. The corpus gate now counts
these too: 3 names across the trees, all copies of one `import_role: name: '{{ role }}'`
playbook. It draws no warning because of a colon-less `# noqa syntax-check[specific]` on the
line, which reads as a bare `# noqa`. Without that comment it gets the existing "never defined"
warning.
