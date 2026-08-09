# T-155 — loop_control with no loop is dead config, and ansible never says so

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P1       | S    | T-099 | —          |

## Problem

A task carrying `loop_control:` but no `loop:` and no `with_*` runs clean. Live-verified on
**ansible-core 2.21.2**:

```yaml
- name: loop_control with no loop at all
  ansible.builtin.debug:
    msg: ran
  loop_control:
    loop_var: thing
    index_var: i
    label: "{{ thing }}"
```

```
TASK [loop_control with no loop at all] ****
ok: [localhost] => { "msg": "ran" }
localhost : ok=1  changed=0  failed=0        # exit 0, not one warning
```

Every key in that block is inert. `loop_var` names nothing, `label` is never rendered, and
the author almost certainly believes otherwise — the usual way in is deleting or renaming the
`loop:` above it and leaving its control block behind.

The silence is structural, not a decision:

| Layer     | What it checks                                                        | Cite                        |
| --------- | --------------------------------------------------------------------- | --------------------------- |
| load time | `loop_control` is a dict, and its keys are real `LoopControl` attrs   | `task.py:87,346-352`        |
| run time  | `loop_var`/`index_var` collisions — reached only via `_run_loop()`    | `task_executor.py:194-206,257-298` |

`loop_control` is a `NonInheritableFieldAttribute` with a default instance, validated in
isolation; nothing cross-references a loop. The one cross-check that exists lives on the
execution path behind `_run_loop()`, which no-loop tasks never enter. So the block is checked
for shape and never for purpose.

The sub-key schema *is* enforced even with no loop — `loop_control: {not_a_real_key: 1}` is
fatal with "'not_a_real_key' is not a valid attribute for a LoopControl" (`base.py:220`),
verified same run. That half is T-108's nested-keyword-table work, not ours.

## Approach

Pure shape test on the semantic AST, same pass as T-110 and no resolution needed: a task node
that has `loop_control` and neither `loop` nor any `loop_with`/`with_*`. Warning, not error —
ansible runs it, so calling it fatal would be us lying in the other direction.

Worth naming the specific keys present in the message ("`loop_var`, `label` have no effect")
rather than a bare "unused block", since that is the sentence that tells the author what they
lost.

`import_role`/`include_role` carry `loop_control` in their valid-keyword sets
(`task_include.py:42`), so the rule must read the *effective* task, not just literal task
keys — an `include_tasks` with `loop_control` and no loop is the same dead block.

## Done when

- [x] a task with `loop_control:` and no `loop:`/`with_*` warns (`task.py:87` — no cross-check exists upstream)
- [x] the message names the inert keys, not just the block
- [x] the same task **with** a loop does not warn, including the `with_*` spelling (`task.py:252-261`)
- [x] `include_tasks`/`include_role` carrying `loop_control` and no loop warns too (`task_include.py:42`)
- [x] severity is WARNING — ansible exits 0 on this, measured on 2.21.2
- [x] `# noqa` suppression works, per T-010
- [x] a fixture exercises both sides: dead block, and a live loop that must stay quiet

## Outcome

Shipped in `placement.rs` on rule id `dead-loop-control`, alongside T-110 row 21 — both hang
off one read of the `loop_control:` key, so the malformed case and the dead case are decided
together.

Two things the measurements settled that the ticket had not
(`scratchpad/t110_row20_21_t155.sh`):

- **A valueless `loop:` still counts as no loop**, so `loop:` + `loop_control:` is dead too —
  the guard upstream is `is not None`, the same one row 10 turns on.
- **Blocks are excluded.** `loop_control` is not a Block keyword at all, so a Block carrying
  one gets `'loop_control' is not a valid attribute for a Block` from T-107, not this warning.
  One fault, one rule; asserted on both sides so neither can silently take the other's case.

The four task shapes measured clean upstream — plain task, `include_tasks`, `include_role`,
`import_tasks` — all warn here.
