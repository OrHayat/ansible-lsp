# T-159 — Task-level invalid-attribute fires where ansible reports a conflicting action

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P2       | M    | T-106 | —          |

## Symptom

A task with a module and one unknown key now gets **two** diagnostics where ansible-core gives
one:

```yaml
- debug: {msg: x}
  nmae: oops        # a typo for `name`
```

| source                        | message                                          |
| ----------------------------- | ------------------------------------------------ |
| ours, `attributes.rs` (T-107) | `'nmae' is not a valid attribute for a Task`     |
| ours, `placement.rs` row 12   | `conflicting action statements: debug, nmae`     |
| **ansible-core 2.21.2**       | `conflicting action statements: debug, nmae` — only this |

The second is right. The first is a message ansible never produces for this input, and it is
not a near-miss: upstream never reaches attribute validation, because `ModuleArgsParser` raises
in `load_list_of_tasks` before `Task.load` runs at all.

Surfaced by T-110 row 12. Before that row shipped we emitted only the wrong one, so this was a
silent divergence rather than a duplicate — the duplicate is what makes it visible.

## Cause

`'%s' is not a valid attribute for a %s` (`base.py:211-220`) can only fire on a key that
survived `ModuleArgsParser`. That parser removes from consideration every key in
`_task_attrs` — `Task.fattributes | Handler.fattributes | {local_action, static}`
(`mod_args.py:128-132`) — and treats **everything else** as a module candidate
(`mod_args.py:330-354`), because `load_list_of_tasks` passes `skip_action_validation=True`.

So on a task that already has a module, an unrecognised key is a *second module*, not a bad
attribute. Measured on 2.21.2, all with a `debug:` already present:

| second key                | ansible says                                     |
| ------------------------- | ------------------------------------------------ |
| `nmae: foo` (typo)        | `conflicting action statements: debug, nmae`     |
| `whne: true` (typo)       | `conflicting action statements: debug, whne`     |
| `gather_facts: false`     | `conflicting action statements: debug, gather_facts` |
| `frobnicate: y`           | `conflicting action statements: debug, frobnicate` |
| `hosts: web`              | `conflicting action statements: debug, hosts`    |
| `user: alice`             | `conflicting action statements: debug, user`     |
| `listen: x`               | **`'listen' is not a valid attribute for a Task`** |
| `static: yes`             | **`'static' is not a valid attribute for a Task`** |

Only the last two produce the attribute message, and for the same reason: both are in
`_task_attrs` (Handler contributes `listen`, `mod_args.py:131` adds `static`) so neither is a
candidate, and both then fail Task validation. That is the entire set. Every other key on a
task with a module is a conflicting action.

Our `ast::build_task` computes `unknown_keys` with `KeyContext::Task` after excluding only the
*first* action candidate as the module, so every later candidate lands in `unknown_keys` and
`attributes.rs` reports it.

## Fix

Keys that are action candidates are row 12's, not the keyword rule's. `build_task` should drop
them from `unknown_keys` rather than reporting them, leaving `'x' is not a valid attribute for
a Task` for the cases that genuinely produce it upstream — `listen`, `static`, and any other
Handler-only attribute on a plain task.

Two things to settle while doing it, neither obvious:

- **The gap where row 12 defers.** Row 12 does not fire when the first candidate's value is one
  `_normalize_parameters` rejects (a list), because upstream raises `unexpected parameter type
  in action` there instead. Dropping the key from `unknown_keys` unconditionally turns that
  into silence. Either row 12 grows the parameter-type message too, or the drop is conditional
  on row 12 having fired.
- **Handler context.** In `KeyContext::Handler` the legal set includes `listen`, so the same
  key is fine there and only `static` remains. The two contexts need separate expectations.

Worth checking the same question for `KeyContext::Block` and `Play` before assuming this is
Task-only: a block has no module slot, so an unknown key there really is an invalid attribute
and `attributes.rs` is right — but that should be measured rather than assumed, given this one
went the other way.

## Resolution

Measured 21 shapes on 2.21.2 with `scratchpad/t159/probe.sh` (`--syntax-check`, one file each).
Two findings went past the ticket:

- `action: debug` or `local_action: debug` beside a typo is also `conflicting action statements:
  debug, nmae`. Row 12 used to require two candidates, so this shape got only the wrong
  attribute message.
- Once the args parser raises, nothing else on the task is reported: `debug:` + `nmae:` +
  `listen:` gives only the conflict, in either order. So the right fix is not "drop candidate
  keys" but "no attribute check on a task the parser rejects".

Changes:

- `placement::action_walk` follows upstream's walk candidate by candidate. An action already
  set (by `action:`/`local_action:` or an earlier candidate) makes the next candidate a
  conflict. Otherwise a list value raises `unexpected parameter type in action: a list`, alone
  or not, which closes the gap where row 12 used to defer into silence. A bare int also raises
  upstream and is still missed, because the parser keeps no scalar style (T-162).
- `placement::args_parser_raises` (rows 11, 12 and 22) is the single answer, and
  `ast::build_task` leaves `unknown_keys` empty when it is true.
- Block and Play were measured and are unaffected: `'nmae' is not a valid attribute for a
  Block/Play`.

Tests: `an_extra_key_gets_exactly_the_one_error_ansible_gives` runs 17 measured shapes through
both rules and asserts the exact message set for each, Task, Handler, Block and Play included.
It was seen red under both halves of the fix reverted separately. Two tests that pinned the old
answers were corrected: the handler case of `the_class_in_the_message_matches_ansibles` (now
`static:`, measured), and the deferral test, which now asserts the parameter-type message. The
`demo/placement.yml` row that called that message "a different rule's" is now a BAD row, and
the demo test lists it.

## Done when

- [x] a typo'd key on a task with a module produces **one** diagnostic, ansible's own
      `conflicting action statements: %s, %s`
- [x] `listen:` and `static:` on a plain task still produce `'%s' is not a valid attribute for
      a Task`, which is what upstream gives — asserted for both
- [x] the row-12-defers gap is closed or documented, not left as new silence. A list is closed;
      a bare int is documented (T-162)
- [x] Block and Play contexts measured and confirmed unaffected, or filed separately
- [x] `demo/invalid_attributes.yml` and `demo/placement.yml` agree on which rule owns which key.
      Both demo tests assert exact lists, and the `every_other_demo_file` guard is green
