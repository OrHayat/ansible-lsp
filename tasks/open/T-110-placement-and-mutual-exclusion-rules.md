# T-110 — Placement and mutual-exclusion rules

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| partly done | task | P1  | M    | T-106 | T-107      |

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

Landing in batches grouped by the context each rule needs, not by severity:

| Batch | Rows            | Shared machinery                    | State                                    |
| ----- | --------------- | ----------------------------------- | ---------------------------------------- |
| 1     | 8, 13-19        | the Play / playbook-entry node      | **done** — `placement.rs`                |
| 2     | 3, 4, 10, 20-21 | one task's loop / `loop_control`    | 3 and 4 **done**; 10, 20-21 open         |
| 3     | 1, 2, 6         | "am I inside `handlers:`" as a flag | open                                     |
| 4     | 5, 9, 23-24     | file-level / include-entry shapes   | open                                     |
| 5     | 11, 12, 22      | the module/args split               | open — really T-046's problem            |

Batch 5 is a candidate to split off: rows 12 and 22 need "which keys resolve to a module",
which is resolution, not shape, so it does not share this ticket's premise.

Batch 1 shipped as `placement.rs`, rule id `invalid-placement`, one diagnostic per fault
rather than Ansible's stop-at-the-first. Every message was measured by running the case
through `--syntax-check` on 2.21.2 (`scratchpad/t110_batch1.sh`), and `demo/placement.yml`
carries a good-and-bad example of each. One deliberate miss, pinned by a test: `hosts: 42`
is fatal to Ansible as an int, but the parse tree keeps no scalar style, so it cannot be
told from the good `hosts: "42"` — a miss, never a false error.

Rows 3-4 followed, on the same measured footing (`scratchpad/t110_loop_on_import.sh`,
`scratchpad/t110_loop_containers.sh`). Three facts that had to be measured rather than
assumed: `with_*` counts, because `preprocess_data` folds it into `loop` before the check;
`loop:` written with **no value** passes, since the test is `task.loop is not None`, while
`loop: []` fails; and the messages are literal strings upstream, so an FQCN import still
reports the bare `import_tasks`. Every task container — all four play-level lists, nested
blocks, and a standalone task file — reaches the same `load_list_of_tasks` and was verified
to fire. Known miss: the `action: import_tasks` spelling, since the module is read from the
written key only.

The `with_` prefix is matched wholesale rather than from a list. All fourteen documented
lookup loops were measured and every one fires (`scratchpad/t110_with_variants.sh`), but the
prefix over-reaches by exactly one case: Ansible folds `with_x` into `loop` only when `x`
names an **installed lookup** (`task.py:336`), so `with_frobnicate` — or a typo'd
`with_item` — is `'with_frobnicate' is not a valid attribute for a TaskInclude` upstream and
a loop error to us. Same line, same severity, different reason; pinned by a test. T-115's
lookup index is what would tighten it, and this is the same leniency
`keywords::is_task_directive` already documents.

## Done when

One box per row of the table, each carrying the cite it was measured from. Rows 1-22 are
ERROR with the message Ansible itself gives; rows 23-24 are WARNING and must not be errors.

- [ ] 1 — `block:` used as a handler (`helpers.py:104-106`)
- [ ] 2 — `include_role`/`import_role` inside `handlers:` (`helpers.py:245-247`)
- [x] 3 — `loop:`/`with_*` on `import_tasks` (`helpers.py:152-154`)
- [x] 4 — `loop:` on `import_role` (`helpers.py:258-260`)
- [ ] 5 — an imported file that is not a list of tasks (`helpers.py:213-214`)
- [ ] 6 — `meta: end_role` outside a role, or in a handler (`helpers.py:280-285`)
- [ ] 7 — `rescue:`/`always:` without `block:` (`block.py:138-142`)
- [ ] 8 — playbook not a list, empty, or an entry that is not a dict (`playbook/__init__.py:74-91`)
      — the not-a-dict entry ships; the other three need to know the file IS a playbook,
      which only the command line says, so they stay unchecked rather than false-positive on
      every vars file. Reopen if T-150's file-kind matrix gives us that knowledge.
- [ ] 9 — both `import_playbook:` and `ansible.builtin.import_playbook:` in one entry (`playbook_include.py:41-48`)
- [ ] 10 — `loop` and a `with_*` together, or two `with_*` (`task.py:252-261`)
- [ ] 11 — `action:` and `local_action:` together (`mod_args.py:322`)
- [ ] 12 — two resolvable module keys in one task (`mod_args.py:353-354`)
- [x] 13 — both `user:` and `remote_user:` in one play (`play.py:170`)
- [x] 14 — `hosts:` empty (`play.py:123`)
- [x] 15 — `hosts:` entry is `None` (`play.py:129`)
- [x] 16 — `hosts:` entry is not a valid host value (`play.py:131`)
- [x] 17 — `hosts:` not a sequence or string (`play.py:134`)
- [x] 18 — a `vars_prompt` entry missing `name:` (`play.py:243`)
- [x] 19 — a `vars_prompt` entry with an unsupported key (`play.py:246`)
- [ ] 20 — `with_x:` with a null value (`task.py:259`)
- [ ] 21 — `loop_control:` whose value is not a dict (`task.py:346-352`)
- [ ] 22 — a task with no module/action at all (`mod_args.py:368`)
- [ ] 23 — an empty imported file WARNS and continues; an empty role `tasks/main.yml` stays silent (`helpers.py:210-212`)
- [ ] 24 — `local_action:` overwriting an explicit `delegate_to:` WARNS (`mod_args.py:303,324`)
- [ ] `import_playbook:` inside a `tasks:` list (`modules/import_playbook.py:57-64`)
- [ ] a fixture file exercises every row above, good and bad
