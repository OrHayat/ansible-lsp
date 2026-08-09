# T-110 — Placement and mutual-exclusion rules

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P1       | M    | T-106 | T-107      |

## Problem

Structural rules that a per-keyword legal set cannot express — each one fatal at load, each
one a closed check against the AST. Nearly all live in `playbook/helpers.py`:

Measured against **ansible-core 2.21.2** by sweeping every `raise Ansible*Error` under
`playbook/` and `parsing/mod_args.py`. Rows 13-22 came out of that sweep; the first twelve
predate it and were re-checked against the same tree.

| #  | Rule                                                                        | Severity | Cite                         | Message                                                                        |
| -- | --------------------------------------------------------------------------- | -------- | ---------------------------- | ------------------------------------------------------------------------------ |
| 1  | `block:` used as a handler                                                  | error    | `helpers.py:104-106`         | Using a block as a handler is not supported.                                   |
| 2  | `include_role`/`import_role` inside `handlers:`                             | error    | `helpers.py:245-247`         | Using '%s' as a handler is not supported.                                      |
| 3  | `loop:`/`with_*` on `import_tasks`                                          | error    | `helpers.py:152-154`         | You cannot use loops on 'import_tasks' statements. Use 'include_tasks' instead.|
| 4  | `loop:` on `import_role`                                                    | error    | `helpers.py:258-260`         | You cannot use loops on 'import_role' statements. Use 'include_role' instead.  |
| 5  | an imported file that is not a list of tasks                                | error    | `helpers.py:213-214`         | included task files must contain a list of tasks                               |
| 6  | `meta: end_role` outside a role, or in a handler                            | error    | `helpers.py:280-285`         | Cannot execute 'end_role' from a handler / from outside of a role              |
| 7  | `rescue:`/`always:` without `block:`                                        | error    | `block.py:138-142`           | '%s' keyword cannot be used without 'block'                                    |
| 8  | playbook not a list, empty, or an entry that is not a dict                  | error    | `playbook/__init__.py:74-91` | four distinct messages — empty / not a list / no plays / not a play-or-import  |
| 9  | both `import_playbook:` and `ansible.builtin.import_playbook:` in one entry | error    | `playbook_include.py:41-48`  | Found conflicting import_playbook actions: %s                                  |
| 10 | `loop` and a `with_*` together, or two `with_*`                             | error    | `task.py:252-261`            | duplicate loop in task: %s                                                     |
| 11 | `action:` and `local_action:` together                                      | error    | `mod_args.py:322`            | action and local_action are mutually exclusive                                 |
| 12 | two resolvable module keys in one task                                      | error    | `mod_args.py:353-354`        | conflicting action statements: %s, %s                                          |
| 13 | both `user:` and `remote_user:` in one play                                 | error    | `play.py:170`                | both 'user' and 'remote_user' are set for this play                            |
| 14 | `hosts:` empty                                                              | error    | `play.py:123`                | Hosts list cannot be empty. Please check your playbook                         |
| 15 | `hosts:` entry is `None`                                                    | error    | `play.py:129`                | Hosts list cannot contain values of 'None'. Please check your playbook         |
| 16 | `hosts:` entry is not a valid host value                                    | error    | `play.py:131`                | Hosts list contains an invalid host value: '%s'                                |
| 17 | `hosts:` not a sequence or string                                           | error    | `play.py:134`                | Hosts list must be a sequence or string. Please check your playbook.           |
| 18 | a `vars_prompt` entry missing `name:`                                       | error    | `play.py:243`                | Invalid vars_prompt data structure, missing 'name' key                         |
| 19 | a `vars_prompt` entry with an unsupported key                               | error    | `play.py:246`                | Invalid vars_prompt data structure, found unsupported key '%s'                 |
| 20 | `with_x:` with a null value                                                 | error    | `task.py:259`                | you must specify a value when using %s                                         |
| 21 | `loop_control:` whose value is not a dict                                   | error    | `task.py:346-352`            | the `loop_control` value must be specified as a dictionary and cannot be a variable itself |
| 22 | a task with no module/action at all                                         | error    | `mod_args.py:368`            | no module/action detected in task.                                             |
| 23 | an imported file that is empty — warns and continues                        | warning  | `helpers.py:210-212`         | —                                                                              |
| 24 | `local_action:` silently overwrites an explicit `delegate_to:`              | warning  | `mod_args.py:303,324`        | — (silent today; ours would be the only warning)                               |

Row 23 has an asymmetric twin worth knowing: an empty role `tasks/main.yml` is fully silent,
so the warning is scoped to imports, not to every empty task file.

Three rules the sweep turned up that are **not** T-110's — they are value-typing, not
placement, and belong to their own tickets: `omit` in `action:` (`task.py:187`) and
`validate_argspec` not a bool or string (`play.py:407`) are T-108; an invalid `register:`
name (`task.py:375`) is T-103. `role_include.py:135-159` and `task_include.py:72-95` are
T-101's, already closed.

`loop_control:` with **no loop at all** is not here on purpose — measured on 2.21.2, ansible
runs it clean, exit 0, no warning. That is dead config nobody flags, so it is a T-099 child,
not a placement rule.

`import_playbook` inside a `tasks:` list is documented as broken in its own EXAMPLES —
"This DOES NOT WORK ... because I'm inside a play already" (`modules/import_playbook.py:57-64`)
— which makes it a placement rule too.

## Approach

One pass over the semantic AST from T-044. No resolution, no index; each rule is a shape test
on a node and its parent.

## Done when

One box per row of the table, each carrying the cite it was measured from. Rows 1-22 are
ERROR with the message Ansible itself gives; rows 23-24 are WARNING and must not be errors.

- [ ] 1 — `block:` used as a handler (`helpers.py:104-106`)
- [ ] 2 — `include_role`/`import_role` inside `handlers:` (`helpers.py:245-247`)
- [ ] 3 — `loop:`/`with_*` on `import_tasks` (`helpers.py:152-154`)
- [ ] 4 — `loop:` on `import_role` (`helpers.py:258-260`)
- [ ] 5 — an imported file that is not a list of tasks (`helpers.py:213-214`)
- [ ] 6 — `meta: end_role` outside a role, or in a handler (`helpers.py:280-285`)
- [ ] 7 — `rescue:`/`always:` without `block:` (`block.py:138-142`)
- [ ] 8 — playbook not a list, empty, or an entry that is not a dict (`playbook/__init__.py:74-91`)
- [ ] 9 — both `import_playbook:` and `ansible.builtin.import_playbook:` in one entry (`playbook_include.py:41-48`)
- [ ] 10 — `loop` and a `with_*` together, or two `with_*` (`task.py:252-261`)
- [ ] 11 — `action:` and `local_action:` together (`mod_args.py:322`)
- [ ] 12 — two resolvable module keys in one task (`mod_args.py:353-354`)
- [ ] 13 — both `user:` and `remote_user:` in one play (`play.py:170`)
- [ ] 14 — `hosts:` empty (`play.py:123`)
- [ ] 15 — `hosts:` entry is `None` (`play.py:129`)
- [ ] 16 — `hosts:` entry is not a valid host value (`play.py:131`)
- [ ] 17 — `hosts:` not a sequence or string (`play.py:134`)
- [ ] 18 — a `vars_prompt` entry missing `name:` (`play.py:243`)
- [ ] 19 — a `vars_prompt` entry with an unsupported key (`play.py:246`)
- [ ] 20 — `with_x:` with a null value (`task.py:259`)
- [ ] 21 — `loop_control:` whose value is not a dict (`task.py:346-352`)
- [ ] 22 — a task with no module/action at all (`mod_args.py:368`)
- [ ] 23 — an empty imported file WARNS and continues; an empty role `tasks/main.yml` stays silent (`helpers.py:210-212`)
- [ ] 24 — `local_action:` overwriting an explicit `delegate_to:` WARNS (`mod_args.py:303,324`)
- [ ] `import_playbook:` inside a `tasks:` list (`modules/import_playbook.py:57-64`)
- [ ] a fixture file exercises every row above, good and bad
