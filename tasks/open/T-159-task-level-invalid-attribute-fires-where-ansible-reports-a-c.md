# T-159 — Task-level invalid-attribute fires where ansible reports a conflicting action

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P2       | M    | T-106 | —          |

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

## Done when

- [ ] a typo'd key on a task with a module produces **one** diagnostic, ansible's own
      `conflicting action statements: %s, %s`
- [ ] `listen:` and `static:` on a plain task still produce `'%s' is not a valid attribute for
      a Task`, which is what upstream gives — asserted for both
- [ ] the row-12-defers gap is closed or documented, not left as new silence
- [ ] Block and Play contexts measured and confirmed unaffected, or filed separately
- [ ] `demo/invalid_attributes.yml` and `demo/placement.yml` agree on which rule owns which key
