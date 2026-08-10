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
| 1  | a block **nested inside** a handler's `block:`/`rescue:`/`always:`          | error    | `helpers.py:104-106`         | Using a block as a handler is not supported.                                   |
| 2  | `include_role`/`import_role` inside `handlers:`                             | error    | `helpers.py:245-247`         | Using '%s' as a handler is not supported. — `%s` is the action **as written**, FQCN included |
| 3  | `loop:`/`with_*` on `import_tasks`                                          | error    | `helpers.py:152-154`         | You cannot use loops on 'import_tasks' statements. Use 'include_tasks' instead.|
| 4  | `loop:` on `import_role`                                                    | error    | `helpers.py:258-260`         | You cannot use loops on 'import_role' statements. Use 'include_role' instead.  |
| 5  | an imported file that is not a list of tasks                                | error    | `helpers.py:213-214`         | included task files must contain a list of tasks                               |
| 6  | `meta: end_role` outside a role, or in a play's `handlers:`                 | error    | `helpers.py:280-285`         | Cannot execute 'end_role' from a handler / from outside of a role              |
| 25 | `meta: flush_handlers` used as a handler                                    | error    | `strategy/__init__.py:883`   | flush_handlers cannot be used as a handler                                     |
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
| 2     | 3, 4, 10, 20-21 | one task's loop / `loop_control`    | **done** — T-155 landed with row 21      |
| —     | 7               | the block classifier itself         | **done** — see below; belonged to no batch |
| 3     | 1, 2, 6, 25     | "am I inside `handlers:`" as a flag | row 1 done (`Pos`); 2, 25, 6 open        |
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

Rows 20-21 closed batch 2, measured in `scratchpad/t110_row20_21_t155.sh`. Row 20 fires only
on a genuinely absent value — `with_items: ""` and `with_items: []` both run clean, and the
parser's `Node::Null`-vs-empty-scalar distinction is what makes that decidable. Row 21 turned
out **stricter** than the loop guard it sits beside: a valueless `loop_control:` is fatal
where a valueless `loop:` is not, and a templated scalar is fatal too, which is what the
message's "cannot be a variable itself" is about. T-155 shipped on the same read of the key.

### Row 7 came first, because the classifier under it was wrong

Row 7 sat in the table and in the done-when list but in **no batch** — and that was not an
oversight of the batching so much as a symptom. It is the one rule that cannot be written
without first fixing what counts as a block, and the bug it exposed was ours, not Ansible's.

`Block.is_block` is **any** of `block`/`rescue`/`always` (`block.py:91-98`). Both
`ast::build_stmt` and `placement::stmt` tested `block:` alone, so a mapping with a `rescue:`
and no `block:` classified as a **task**. Worse than a mislabel: `find_action` then took
`rescue` for the module name, which left `unknown_keys` empty, so the node read as a
perfectly well-formed task and *every* rule downstream stayed silent on it. Measured against
2.21.2 on five demo files, we were wrong on four — two wrong messages, two silences — and
right only on the control that had a real `block:`. `is_task_directive` never learned
`BLOCK_OWN` the way `is_block_directive` did four lines below it (`keywords.rs:314-329`);
fixing the classifier routes these nodes to `build_block`, so `find_action` never sees them
and the phantom module goes away as a side effect.

Edges, all measured: the guard is `if value and not self.block` — Python truthiness both
sides — so an empty `block: []` counts as no block and still fires, while an empty
`rescue: []` is no fault at all. A null value for any of the three is `_load`'s own
`A malformed block was encountered...`, a different message and not this rule's. On a
duplicate key the **last** wins, in both directions, which is what `Node::get` already does.
`rescue` is reported before `always` regardless of written order — FieldAttribute
declaration order, not the document's — and Ansible stops at the first, so one node yields
one diagnostic.

One deliberate divergence, pinned by `a_nested_rescue_only_mapping_is_a_block_too`: upstream
classifies the same mapping differently depending on nesting depth, and gives an internal
class name for the nested spelling. We give row 7's message at every depth instead. That is
an upstream bug rather than a rule we owe —
`upstream/ansible-nested-block-classification.md` has the measurements and the one-line fix.

### Batch 3, scoped before writing any of it

Measured in `scratchpad/t110_batch3_handlers.sh` and
`scratchpad/t110_row1_block_handler.sh`. Two rows in the original table were wrong, and one
rule was missing:

- **Row 1 was three-quarters wrong.** A top-level `block:` in `handlers:` does **not** error:
  `Play._load_handlers` calls `load_list_of_blocks` (`play.py:205`), which loads a block
  itself and never consults `use_handlers`. The raise is in `load_list_of_tasks`, so it is
  reachable only for a block **nested** inside a handler's `block:`/`rescue:`/`always:`. A
  block pulled in by `include_tasks:` from a handler is fine too.
- **Row 2's `%s` is the action as written**, FQCN included — measured
  `Using 'ansible.builtin.include_role' as a handler is not supported.` This is unlike rows
  3-4, whose messages are literals, so the two cannot share a formatter.
- **Row 25 is new:** `meta: flush_handlers` as a handler has its own error, raised at
  strategy level rather than at load.
- **Row 6b** is decidable without the handler flag: `meta: end_role` errors in a play's
  `tasks:`, passes inside a role, and passes in a role's `handlers/` file. Only the play-level
  handler list triggers 6a. It still needs role membership, which is a path fact.

What a handler list accepts, measured end to end: `include_tasks:` yes; `import_tasks:` and a
top-level `block:` load but destroy the handler's name, so `notify:` can never find it;
`include_role`/`import_role` refused; `meta: end_role` and `meta: flush_handlers` refused.

That middle pair is a **T-099 candidate**: the handler silently becomes unreachable, and
Ansible only complains at run time, and only if the notifying task changed — which is exactly
the gap `upstream/ansible-missing-handler.md` already documents. File it as a child of T-099
rather than a row here, since Ansible gives no load-time error to replicate.

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

- [x] 1 — a block **nested inside** a handler's `block:`/`rescue:`/`always:` (`helpers.py:104-106`)
      — one diagnostic per handler however deep the stack, since Ansible stops at the
      outermost. Missed in a standalone file: a role's `handlers/main.yml` fires it upstream,
      but content cannot tell that file from `tasks/main.yml`, where the nesting is legal.
      Liftable with T-150's file-kind matrix.
- [ ] 2 — `include_role`/`import_role` inside `handlers:`, message naming the action as written (`helpers.py:245-247`)
- [ ] 25 — `meta: flush_handlers` used as a handler (`strategy/__init__.py:883`)
- [x] 3 — `loop:`/`with_*` on `import_tasks` (`helpers.py:152-154`)
- [x] 4 — `loop:` on `import_role` (`helpers.py:258-260`)
- [ ] 5 — an imported file that is not a list of tasks (`helpers.py:213-214`)
- [ ] 6 — `meta: end_role` outside a role, or in a handler (`helpers.py:280-285`)
- [x] 7 — `rescue:`/`always:` without `block:` (`block.py:138-142`) — the guard is
      `if value and not self.block`, so an empty `block: []` still fires and an empty
      `rescue: []` does not; a null value is `_load`'s own message, not this rule's
- [ ] 8 — playbook not a list, empty, or an entry that is not a dict (`playbook/__init__.py:74-91`)
      — the not-a-dict entry ships; the other three need to know the file IS a playbook,
      which only the command line says, so they stay unchecked rather than false-positive on
      every vars file. Reopen if T-150's file-kind matrix gives us that knowledge.
- [ ] 9 — both `import_playbook:` and `ansible.builtin.import_playbook:` in one entry (`playbook_include.py:41-48`)
- [x] 10 — `loop` and a `with_*` together, or two `with_*` (`task.py:252-261`)
      — the rule is **asymmetric upstream**: `loop:` then `with_*` is fatal, `with_*` then
      `loop:` runs clean and runs wrong. Filed as `upstream/ansible-duplicate-loop.md`. We
      give Ansible's error for the fatal order, and a warning of our own
      (`shadowed-loop`, the module's one deliberate divergence) for the accepted one.
- [ ] 11 — `action:` and `local_action:` together (`mod_args.py:322`)
- [ ] 12 — two resolvable module keys in one task (`mod_args.py:353-354`)
- [x] 13 — both `user:` and `remote_user:` in one play (`play.py:170`)
- [x] 14 — `hosts:` empty (`play.py:123`)
- [x] 15 — `hosts:` entry is `None` (`play.py:129`)
- [x] 16 — `hosts:` entry is not a valid host value (`play.py:131`)
- [x] 17 — `hosts:` not a sequence or string (`play.py:134`)
- [x] 18 — a `vars_prompt` entry missing `name:` (`play.py:243`)
- [x] 19 — a `vars_prompt` entry with an unsupported key (`play.py:246`)
- [x] 20 — `with_x:` with a null value (`task.py:259`) — only a *missing* value counts;
      `with_items: ""` and `with_items: []` both run clean upstream
- [x] 21 — `loop_control:` whose value is not a dict (`task.py:346-352`) — stricter than the
      `loop:` guard: a valueless `loop_control:` is fatal too, and so is a templated scalar
- [ ] 22 — a task with no module/action at all (`mod_args.py:368`)
- [ ] 23 — an empty imported file WARNS and continues; an empty role `tasks/main.yml` stays silent (`helpers.py:210-212`)
- [ ] 24 — `local_action:` overwriting an explicit `delegate_to:` WARNS (`mod_args.py:303,324`)
- [ ] `import_playbook:` inside a `tasks:` list (`modules/import_playbook.py:57-64`)
- [ ] a fixture file exercises every row above, good and bad
