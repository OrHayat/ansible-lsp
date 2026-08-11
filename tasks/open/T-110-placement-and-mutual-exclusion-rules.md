# T-110 — Placement and mutual-exclusion rules

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| partly done | task | P1  | M    | T-106 | T-107, T-162, T-163 |

## Problem

Structural rules that a per-keyword legal set cannot express — each one a closed check against
the AST, nearly all of them in `playbook/helpers.py`.

Found by sweeping every `raise Ansible*Error` under `playbook/` and `parsing/mod_args.py` on
**ansible-core 2.21.2**. Rows 13-22 came out of that sweep; the first twelve predate it and were
re-checked against the same tree. Rows 26-30 came later and are the reason to distrust the sweep
as a completeness check: 26 is an `AnsibleAssertionError` re-raised through `Block._load`, 29 and
30 are a `_display.warning` and two uncaught `TypeError`s. None matches `raise Ansible*Error`.

## The rules

One row per rule, in numeric order. **Tier** is what we emit. **Shape** is the kind of question
the rule asks, which is what decides how it is written:

`pos` = where the node sits · `co-occur` = two keys of one node · `value` = one key's value ·
`required` = a key that must be present · `keyset` = a closed vocabulary · `doc` = the shape of a
whole file.

The messages are not repeated here: they live as constants in `placement.rs` and
`include_target.rs`, are asserted verbatim by the LSP tests, and each has a worked example in
`demo/placement.yml` or `demo/include_targets.yml`.

| #   | Rule                                                    | Tier | Where · Shape                | Upstream                     | Status                                                    |
| --- | ------------------------------------------------------- | ---- | ---------------------------- | ---------------------------- | --------------------------------------------------------- |
| 1   | a block nested inside a handler's `block:`/`rescue:`/`always:` | err | handler body · pos    | `helpers.py:104-106`         | done — `REFUSED`; miss in standalone files (T-150)         |
| 2   | `include_role`/`import_role` inside `handlers:`         | err  | handler · pos                | `helpers.py:245-247`         | done — `REFUSED`; miss: the `action:` form                 |
| 3   | `loop:`/`with_*` on `import_tasks`                      | err  | task · co-occur              | `helpers.py:152-154`         | done — `loop_on_import`; miss: the `action:` form          |
| 4   | `loop:` on `import_role`                                | err  | task · co-occur              | `helpers.py:258-260`         | done — `loop_on_import`                                    |
| 5   | an imported file that is not a list of tasks            | err  | include target · doc         | `helpers.py:213-214`         | done — `include_target.rs`; both spellings                 |
| 6a  | `meta: end_role` in a handler                           | err  | handler · pos                | `helpers.py:278-281`         | done — `REFUSED`; miss in standalone files (T-150)         |
| 6b  | `meta: end_role` outside a role                         | err  | play task list · pos         | `helpers.py:283-285`         | done — `REFUSED`; miss in standalone files (T-068 rule 3)  |
| 7   | `rescue:`/`always:` without `block:`                    | err  | block · co-occur             | `block.py:138-142`           | done — `rescue_without_block`                              |
| 8   | a playbook that is empty, not a list, or holds no plays | err  | playbook document · doc      | `playbook/__init__.py:74-91` | **partly** — 1 of 4 from content alone; the rest are T-163 |
| 8i  | the same three, on an `import_playbook:` target         | err  | import target · doc          | `playbook/__init__.py:74-91` | done — `include_target.rs`; the reference proves the kind  |
| 9   | two `import_playbook` spellings in one entry            | err  | playbook entry · co-occur    | `playbook_include.py:41-48`  | done — `conflicting_import_playbook`; a set, not a pair    |
| 10  | `loop` with a `with_*`, or two `with_*`                 | err  | task · co-occur              | `task.py:252-261`            | done — `duplicate_loop`; the only order-sensitive rule     |
| 11  | `action:` and `local_action:` together                  | err  | task · co-occur              | `mod_args.py:322`            | done — `EXCLUSIONS`; beats the loop rules                  |
| 12  | two module keys in one task                             | err  | task · co-occur              | `mod_args.py:353-354`        | done — `action_walk`; needs no resolution                  |
| 13  | `user:` and `remote_user:` in one play                  | err  | play · co-occur              | `play.py:170`                | done — `EXCLUSIONS`                                        |
| 14  | `hosts:` empty                                          | err  | play · value                 | `play.py:123`                | done — `hosts`                                             |
| 15  | a `hosts:` entry that is `None`                         | err  | play · value                 | `play.py:129`                | done — `hosts`                                             |
| 16  | a `hosts:` entry that is not a valid host value         | err  | play · value                 | `play.py:131`                | done — `hosts`                                             |
| 17  | `hosts:` that is not a sequence or a string             | err  | play · value                 | `play.py:134`                | done — `hosts`; miss: `hosts: 42` (T-162)                  |
| 18  | a `vars_prompt` entry with no `name:`                   | err  | vars_prompt entry · required | `play.py:243`                | done — `vars_prompt`                                       |
| 19  | a `vars_prompt` entry with an unsupported key           | err  | vars_prompt entry · keyset   | `play.py:246`                | done — `vars_prompt`                                       |
| 20  | `with_x:` written with no value                         | err  | task · value                 | `task.py:259`                | done — `duplicate_loop`                                    |
| 21  | `loop_control:` whose value is not a mapping            | err  | task · value                 | `task.py:346-352`            | done — `loop_control_checks`                               |
| 22  | a task with no module/action at all                     | err  | task · required              | `mod_args.py:368`            | done — `action_walk`; needs no resolution                  |
| 23  | an imported file that is empty                          | warn | include target · doc         | `helpers.py:210-212`         | done — `include_target.rs`; ours, `empty-task-file`        |
| 24  | `local_action:` overwriting an explicit `delegate_to:`  | warn | task · co-occur              | `mod_args.py:303,325`        | done — `EXCLUSIONS`; ours, `discarded-delegate-to`         |
| 25  | `meta: flush_handlers` used as a handler                | err  | handler · pos                | `strategy/__init__.py:883`   | done — `REFUSED`; raised at run time, not load             |
| 26  | a task-list entry that is not a mapping                 | err  | any task list · value        | `helpers.py:100-102`         | done — `stmt`; ours, `malformed-task-entry`                |
| 27  | a `vars_prompt` entry that is not a mapping             | err  | vars_prompt entry · value    | `vars/manager.py:102-107`    | done — `vars_prompt`; the same guard, written correctly    |
| 28  | `tags:` that is neither a list nor a string             | err  | play/task/block · value      | `taggable.py:48-55`          | done — `tags_checks`; miss: `tags: 42` (T-162)             |
| 29  | a reserved tag name (`all`, `tagged`, `untagged`)       | warn | play/task/block · value      | `taggable.py:58-59`          | done — `tags_checks`; ours, `reserved-tag-name`            |
| 30a | a `tags:` member that is a list or a mapping            | err  | play/task/block · value      | `taggable.py:58`             | done — `tags_checks`; ours, `invalid-tag-member`           |
| 30b | a `tags:` member that is an `int`                       | warn | play/task/block · value      | `cli/playbook.py:201,206`    | **open** — blocked on T-162, no scalar style               |
| ip  | `import_playbook:` written in a task list               | err  | play task list · pos         | `modules/import_playbook.py:57-64` | done — `REFUSED`; ours, `misplaced-import-playbook`  |

### What is deliberately not here

- **Value typing, not placement.** `omit` in `action:` (`task.py:187`) and a non-bool
  `validate_argspec` (`play.py:407`) are T-108; an invalid `register:` name (`task.py:375`) is
  T-103. `role_include.py:135-159` and `task_include.py:72-95` are T-101's, already closed.
- **`loop_control:` with no loop at all.** Measured clean on 2.21.2 — exit 0, no warning. Dead
  config nobody flags, so it shipped as T-155 on its own rule id rather than as a row here.
- **A handler whose name becomes unnotifiable.** An `import_tasks:` or a top-level `block:` in
  `handlers:` loads, but destroys the handler's name so `notify:` can never find it. No
  load-time error to replicate, so it is a T-099 child (T-157), not a row.

Two facts worth keeping next to the table. Row 23 has an asymmetric twin: an empty role
`tasks/main.yml` is fully silent, so the warning is scoped to imports rather than to every empty
task file. And row `ip` is a placement rule on upstream's own authority — `import_playbook`
inside a `tasks:` list is documented as broken in its own EXAMPLES, *"This DOES NOT WORK ...
because I'm inside a play already"*.

## Approach

One pass over the semantic AST from T-044. No resolution, no index; each rule is a shape test on
a node and its parent. Every message was measured by running the case through `--syntax-check`
on 2.21.2 before it was written, and one diagnostic is emitted per fault rather than Ansible's
stop-at-the-first.

**Only one shape is table-driven, and the `Shape` column says why.** The six `pos` rows all ask
the same question — *what is this node, and where does it sit* — so they are data in `REFUSED`.
No other shape has that property: the `co-occur` rows ask about different key pairs with
different messages, and the `value` rows each test a different thing about a different key. A
"table" over those is a list of closures, which is the same functions with ceremony in front.
What those shapes do support is grouping: rows 14-17 collapse into one `hosts`, 18-19 and 27 into
one `vars_prompt`, 3-4 into one `loop_on_import`, 28-30a into one `tags_checks`.

**Rows 5, 8i and 23 broke the premise and got a second home.** They judge the file a reference
*points at*, which needs a resolved path and a second parse. `include_target.rs` owns them, fed
by the `refs` that analysis already resolves. That turned out to be the interesting part: the
reference is also what *proves* the target's kind — an `import_tasks:` proves a task file, an
`import_playbook:` proves a playbook — which is the question `placement.rs` has to give up on
when judging a standalone file on its own.

**What is left is blocked, and tracked elsewhere.** Row 8's other three cases went to T-163
(they need T-020's reverse index to know a file is an entry point); row 30b went to T-162 (it
needs scalar style, since `tags: [7]` and `tags: ["7"]` are one node to the parser and only the
first misbehaves). Both are named in this ticket's `Depends on`, so the board stops offering
T-110 as available work.

## Done when

One box per row, each carrying the cite it was measured from and whatever the measurement
contradicted.

- [x] 1 — a block **nested inside** a handler's `block:`/`rescue:`/`always:` (`helpers.py:104-106`)
      — one diagnostic per handler however deep the stack, since Ansible stops at the
      outermost. Missed in a standalone file: a role's `handlers/main.yml` fires it upstream,
      but content cannot tell that file from `tasks/main.yml`, where the nesting is legal.
      Liftable with T-150's file-kind matrix.
- [x] 2 — `include_role`/`import_role` inside `handlers:`, message naming the action as written
      (`helpers.py:245-247`) — all six spellings of
      `_ACTION_ALL_PROPER_INCLUDE_IMPORT_ROLES` and no others; applies at a handler's top level
      too, unlike row 1, since a plain entry there is wrapped into an implicit block and
      re-loaded. Beats both loop rules on the same task. Known miss: `action: include_role`, the
      same trade rows 3-4 take.
- [x] 3 — `loop:`/`with_*` on `import_tasks` (`helpers.py:152-154`)
- [x] 4 — `loop:` on `import_role` (`helpers.py:258-260`)
- [x] 5 — an imported file that is not a list of tasks (`helpers.py:213-214`) — and the
      `include_tasks` spelling with it: measured, it gives the *same* message, just as a run-time
      `fatal:` instead of at load. Same fault, later feedback, so it is the more valuable of the
      two to catch in an editor. Fires only where the target is a literal path we resolve.
      Rule id `invalid-task-file`.
- [x] 6 — `meta: end_role` outside a role, or in a handler (`helpers.py:278-285`) — two
      independent raises. 6a needs no role knowledge at all: `use_handlers` short-circuits
      first, so it fires in a role's own `handlers/` file too, which the earlier scoping note
      had backwards. 6b never proves a statement IS in a role — a play's own task list is a
      position where it provably is not, measured still fatal alongside `roles:`. Missed in
      standalone files, and not liftable by T-150: a byte-identical include target is legal
      from a role and fatal from a play, so the file has no answer. Filed as T-068 rule 3 —
      the same chain walk, the same positive-evidence rule.
- [x] 7 — `rescue:`/`always:` without `block:` (`block.py:138-142`) — the guard is
      `if value and not self.block`, so an empty `block: []` still fires and an empty
      `rescue: []` does not; a null value is `_load`'s own message, not this rule's.
- [ ] 8 — a playbook that is empty, not a list, or holds no plays (`playbook/__init__.py:74-91`)
      — the not-a-dict entry ships, because a sibling entry is a real play and so proves the
      file's kind. The other three are exactly the cases with no such proof inside the file.
      Split out as **T-163** (blocked on T-020's reverse index): path and reserved-name
      exclusions get most of the way, and what is left is one shape — an unreferenced,
      unknown-named file that is empty, a bare mapping, or `[]`.
- [x] 8i — the same three faults on a file reached by `import_playbook:`, where the reference
      proves the kind exactly as `import_tasks:` does for rows 5 and 23. Needed neither T-150
      nor T-020. The branch order is **not** the task-file one and deliberately does not share
      `falsy`: a playbook is tested for `None` first and falsiness last, so `{}` is "not a list
      of plays" while `[]` is "no plays" — where a task file calls both empty. Four shapes
      measured. Two ids, `empty-playbook` and `invalid-playbook`; both keep upstream's opening
      sentence and drop its path and `<class ...>` interpolation, the first being already on the
      line we anchor to and the second meaningless in YAML. A task-level `import_playbook:` is
      excluded by `Reference::playbook_entry` — ansible reads it as a module name and never
      opens the file, so row `ip` owns it alone.
- [x] 9 — both `import_playbook:` and `ansible.builtin.import_playbook:` in one entry
      (`playbook_include.py:41-48`) — a **set** of the three spellings, so any two collide and
      duplicates of one do not; the names print sorted, not in document order. Both measured.
- [x] 10 — `loop` and a `with_*` together, or two `with_*` (`task.py:252-261`)
      — the rule is **asymmetric upstream**: `loop:` then `with_*` is fatal, `with_*` then
      `loop:` runs clean and runs wrong. Filed as `upstream/ansible-duplicate-loop.md`. We
      give Ansible's error for the fatal order, and a warning of our own
      (`shadowed-loop`, the module's one deliberate divergence) for the accepted one.
- [x] 11 — `action:` and `local_action:` together (`mod_args.py:322`) — raised by
      `ModuleArgsParser`, which runs before `Task.load`, so it beats the `preprocess_data`
      rules; measured on a task carrying both. A null trigger is a different message.
- [x] 12 — two resolvable module keys in one task (`mod_args.py:353-354`) — **no resolution
      needed**, which this ticket had wrong: `load_list_of_tasks` passes
      `skip_action_validation=True` (`helpers.py:121`), so every non-attribute key is a
      candidate whether or not it names a module. Measured on `frobnicate`, which names none.
      Known miss: a first value `_normalize_parameters` rejects raises there instead.
- [x] 13 — both `user:` and `remote_user:` in one play (`play.py:170`)
- [x] 14 — `hosts:` empty (`play.py:123`)
- [x] 15 — `hosts:` entry is `None` (`play.py:129`)
- [x] 16 — `hosts:` entry is not a valid host value (`play.py:131`)
- [x] 17 — `hosts:` not a sequence or string (`play.py:134`) — one deliberate miss, pinned by a
      test: `hosts: 42` is fatal to Ansible as an int, but the parse tree keeps no scalar style,
      so it cannot be told from the good `hosts: "42"`. A miss, never a false error. T-162.
- [x] 18 — a `vars_prompt` entry missing `name:` (`play.py:243`)
- [x] 19 — a `vars_prompt` entry with an unsupported key (`play.py:246`)
- [x] 20 — `with_x:` with a null value (`task.py:259`) — only a *missing* value counts;
      `with_items: ""` and `with_items: []` both run clean upstream, and the parser's
      `Node::Null`-vs-empty-scalar distinction is what makes that decidable.
- [x] 21 — `loop_control:` whose value is not a dict (`task.py:346-352`) — **stricter** than the
      `loop:` guard it sits beside: a valueless `loop_control:` is fatal where a valueless
      `loop:` is not, and so is a templated scalar, which is what the message's "cannot be a
      variable itself" is about. T-155 shipped on the same read of the key.
- [x] 22 — a task with no module/action at all (`mod_args.py:368`) — the far end of row 12's
      walk, so it needs no resolution either. `action:`/`local_action:` are task attributes and
      never appear as candidates, but each supplies the action from its own branch. The
      neighbouring `couldn't resolve module/action` stays T-046's: it needs the second,
      resolving parse inside `Task.load`.
- [x] 23 — an empty imported file WARNS and continues; an empty role `tasks/main.yml` stays
      silent (`helpers.py:210-212`). Extended to `include_tasks`, which is silent at load **and**
      at run time — measured, the play runs straight past it with no output at all. Rule id
      `empty-task-file`, ours: upstream's message interpolates the resolved path, and we anchor
      on the reference where that path is already written, so ours says what the author gains
      instead — that the line does nothing, and whether ansible would ever have told them.
      "Empty" is Python truthiness, not a byte count: `[]`, `{}` and a comments-only file are
      all this rule, not row 5. Measured, all four warn.
- [x] 24 — `local_action:` overwriting an explicit `delegate_to:` WARNS (`mod_args.py:303,325`)
      — ours, on rule id `discarded-delegate-to`. Proven with a control: `delegate_to: other`
      alone runs `ok: [localhost -> other]`, and the arrow disappears with `local_action`
      beside it.
- [x] 25 — `meta: flush_handlers` used as a handler (`strategy/__init__.py:883`) — raised at
      run time, not load, so `--syntax-check` alone never sees it. Both handler depths.
- [x] 26 — a task-list entry that is not a mapping (`helpers.py:100-102`) — not in the original
      sweep, which caught `raise Ansible*Error` but not this `AnsibleAssertionError` re-raised
      through `Block._load`. Ours, on `malformed-task-entry`: upstream interpolates the whole
      list where it means the entry, so it always reports `<class 'list'>`, and the raise has no
      `obj=` so there is no line number. Filed as `upstream/ansible-malformed-task-entry.md`.
      A bare `-` is excluded — measured clean, dropped by `load_list_of_blocks`.
- [x] 27 — a `vars_prompt` entry that is not a mapping (`vars/manager.py:102-107`) — found by
      asking whether row 26's fault repeats elsewhere. It does, and here it is written the way
      row 26 should have been: an `AnsibleParserError` with `obj=item`, so it names the entry,
      points a caret at it, and adds a help text. Replicated verbatim rather than reworded.
      Scalar, sequence and **null** entries all fatal — the null is the asymmetry worth knowing,
      since the same bare `-` in a task list loads clean. Aliases stay silent until T-160.
- [x] 28 — `tags:` that is neither a list nor a string (`taggable.py:48-55`) — found while
      measuring null-valued keys for T-161. `_load_tags` accepts a list or a comma string and
      nothing else, so a **null** `tags:` is fatal where every other null play key loads clean.
      `Taggable` is mixed into Play, Task, Block and Role; the first three are wired, and a
      `roles:` entry is not walked by this module at all. Known miss, shared with row 17:
      `tags: 42` is one node with `tags: "42"` to a parser that keeps no scalar style, and only
      the first is fatal — so it stays silent rather than flagging the legal spelling.
- [x] 29 — a reserved tag name (`taggable.py:58-59`) — `all`, `tagged`, `untagged`, and the
      comma-string spelling is split before the check so `tags: all,deploy` counts too. **Not**
      a verbatim replication, which the sweep assumed it would be: upstream interpolates
      `list(set_intersection)`, and Python randomises string hashing per process, so five runs
      of one file printed four different orders. There is no single message to copy. Ours sorts
      the names and takes its own id, `reserved-tag-name`.
- [x] 30a — a `tags:` member that is a list or a mapping. Unhashable, so it dies in the
      reserved-name intersection at load, *before* `listof` — the check written to reject it —
      runs in `post_validate`. Ours, on `invalid-tag-member`: upstream says "Unexpected
      Exception, this is probably a bug". Filed as `upstream/ansible-tags-member-types.md`.
- [ ] 30b — a `tags:` member that is an `int`. Declared legal by `listof=(str, int)`, runs
      clean, cannot be selected by `--tags`, and crashes `--list-tasks` and `--list-tags`.
      Wants a WARNING rather than an error, since the play itself works. **Blocked on T-162**:
      `tags: [7]` and `tags: ["7"]` are one node until the parser keeps scalar style, and only
      the first misbehaves — firing on both would be a false error on legal code.
- [x] ip — `import_playbook:` inside a `tasks:` list (`modules/import_playbook.py:57-64`) — the
      message is **ours**, on rule id `misplaced-import-playbook`. Ansible fails, but at run
      time (exit 2) and with a message about *parameters* that never mentions position, and
      which one you get depends on the value's shape — a raw path gives `does not support raw
      params`, a `{file: ...}` mapping gives `module (import_playbook) is missing`. Nothing to
      borrow, so it names the fault and the fix instead.
      It is also the **only** diagnostic on that line, which it was not before row 8i landed:
      the target of a misplaced import was still being resolved, so a missing one produced a
      `missing-file` warning about a lookup ansible never performs. Both that and row 8i now key
      off `Reference::playbook_entry` — one question, "does ansible actually load this target",
      asked in both places.
- [x] a fixture file exercises every row above, good and bad — `demo/placement.yml` and
      `demo/include_targets.yml` are the fixture, and their `# GOOD`/`# BAD` annotations are now
      a *contract*: `every_annotated_demo_line_matches_its_diagnostics` asserts every BAD line
      produces one of this ticket's diagnostics and every GOOD line produces none. The GOOD half
      is what was missing — every other test asserts a rule fires, so a rule that over-fired on
      the legal spelling beside it would have passed the whole suite. A third assertion requires
      each of the ten rule ids to appear somewhere in the fixture; it failed on first run,
      because row 30a had shipped with no demo case at all.

## Implementation notes

Kept because each one records a measurement that contradicted an assumption. They are history,
not instructions — the rules above are the current truth.

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

### The handler rows were scoped before any of them was written

Measured in `scratchpad/t110_batch3_handlers.sh` and `scratchpad/t110_row1_block_handler.sh`.
Two rows in the original table were wrong, and one rule was missing:

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

### The `with_` prefix over-reaches by exactly one case

It is matched wholesale rather than from a list. All fourteen documented lookup loops were
measured and every one fires (`scratchpad/t110_with_variants.sh`), but Ansible folds `with_x`
into `loop` only when `x` names an **installed lookup** (`task.py:336`), so `with_frobnicate` —
or a typo'd `with_item` — is `'with_frobnicate' is not a valid attribute for a TaskInclude`
upstream and a loop error to us. Same line, same severity, different reason; pinned by a test.
T-115's lookup index is what would tighten it, and this is the same leniency
`keywords::is_task_directive` already documents.

### Rows 3-4, on the same measured footing

`scratchpad/t110_loop_on_import.sh`, `scratchpad/t110_loop_containers.sh`. Three facts that had
to be measured rather than assumed: `with_*` counts, because `preprocess_data` folds it into
`loop` before the check; `loop:` written with **no value** passes, since the test is
`task.loop is not None`, while `loop: []` fails; and the messages are literal strings upstream,
so an FQCN import still reports the bare `import_tasks`. Every task container — all four
play-level lists, nested blocks, and a standalone task file — reaches the same
`load_list_of_tasks` and was verified to fire.
