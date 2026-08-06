# T-078 — `when:` explanation and module provenance fight over the module-name token

| Status   | Priority | Size | Epic  | Depends on |
| -------- | -------- | ---- | ----- | ---------- |
| **done** | P3       | S    | T-121 | —          |

## Problem

On a task that has **both** a module and a `when:`, hovering the module name is ambiguous —
the same token yields two different hovers depending on a global setting, and one is always
unreachable.

Repro — `demo/tasks/variables.yml`, the registered-result task:

```yaml
- name: Cmd+click cmd_result, here and in the when, jumps to where it was registered
  ansible.builtin.debug:
    msg: "{{ cmd_result.stdout }}"
  when: cmd_result.rc == 0
```

The task's `when:` is attached to the **module reference** — verified: the extracted
`ansible.builtin.debug` reference carries `conditions=["cmd_result.rc == 0"]`. This attachment
is intentional and load-bearing elsewhere (`task_conditions_attach_to_the_reference` — an
include edge needs to know its guard). The bug is that the **hover** keys off it:

- `main.rs` hover handler (~line 1315): `if settings.hints && !r.conditions.is_empty()` returns
  the `when:` explanation *anchored on the module name*, before the module-provenance branch.
- So with **inlay hints on** (the default), hovering `ansible.builtin.debug` shows
  *"runs only if cmd_result.rc == 0"* and you never see the module provenance.
- With **hints off**, hovering the module name shows the provenance
  (*"…runs on the controller (action plugin)…"*) and the `when:` explanation is unreachable by
  hover entirely.

One token, two meanings, switched by a setting that is about inlay hints — not about hovers.

## Second, related gap

The hover path uses `references::extract`, which yields only file/role/**module** references —
**no variable references**. So hovering a variable inside `when:` (or inside `{{ }}`) finds no
reference and returns nothing, even though go-to-definition on that same variable works (it's
served by the definition provider). `variables.yml` even advertises *"Cmd+click cmd_result,
here and in the when"* — the click works, the hover is silent.

## Approach

- Anchor the `when:` explanation on the **`when:` clause** (its keyword/value token), not on the
  module-name token. Then module-name hover always shows provenance, and the `when:` hover shows
  on the `when:` — each where you'd point for it, independent of the inlay-hints setting.
- Keep `conditions` on the reference (other consumers need it); this is purely a hover-anchoring
  and precedence change.
- Optional, folds in naturally: give variables a hover on the `when:`/`{{ }}` occurrences that
  mirrors go-to-definition (where it's defined / registered), so hover and Cmd+click agree.

## Outcome

**The framing above is half wrong, and the wrong half cost a regression.** "Move the condition
off the reference" is right for a module and wrong for an include: on an include or an
`import_playbook`, "this edge may not be taken" is exactly what you want at the place the edge
is written. Moving it off wholesale left a conditional `include_tasks: sibling.yml` hovering
**nothing at all** — a resolved single-target include is deliberately quiet, so the condition
had been its only hover. Caught by hovering one, not by reasoning.

The real fault was never the anchor. It was that the condition *replaced* the reference hover
instead of **appending** to it. So:

| hover target                     | shows                                                  |
| -------------------------------- | ------------------------------------------------------ |
| a module name                    | provenance **+** `_Conditional_ — <guard>`             |
| an include path / role name      | `→ target` **+** `_Conditional_ — <guard>`             |
| an `import_playbook` path        | both of the above **+** the fan-out footnote           |
| the `when` keyword               | the guard, spelled out in English                      |
| a variable inside the condition  | where it was defined or registered                     |

`guard_line` is deliberately one line; the full `when:` block stays on the clause. A guarded
reference also counts as worth hovering even when a resolved single target otherwise wouldn't
be — "this include may not run" is not a Cmd+click away — and it anchors on a compact
`→ target` line rather than the verbose `Tried:` dump, which stays behind
`candidatesOnResolved`.

**No AST change was needed.** `when` is a directive on tasks, blocks and plays alike, so
`collect_directives` (`ast.rs:217-232`) had already been recording its `key_span` all along;
the reference layer just never carried it. `Reference.condition_key_span` copies it across in
`references.rs`, alongside the `condition_span` (the value) that was already there.

The `settings.hints` gate is gone from the hover path entirely — it gates inlay hints, which is
all it ever claimed to. **The second gap was already closed:** `vars::uses` walks `when:` values
through `expression_uses` (`vars.rs:209-235`), so once the reference branch stopped returning
early, hovering a variable in a condition fell through to `variable_hover_at` and worked. The
ticket's claim that `extract` returning no variable references made it silent was wrong — the
reference branch shadowing the fallthrough was the whole story.

The hover body moved out of the async handler into a free `hover_at(doc, nodes, path, byte,
settings)`. The bug was one of *precedence* between hovers competing for a token, and precedence
is what wants a test; `Backend` needs a live `Client`, so nothing in the old handler was
reachable from one.

### Rejected: also anchoring on the condition's value

Built and compared side by side against the real demo at twelve cursor positions. Anchoring on
the value too — merging the variable's definition and the guard into one tooltip under a
divider, so that an operator or an undefined magic var inside the condition still answers —
closes the two remaining dead spots (`== 0`, and a variable with no definition such as
`inventory_hostname`). It was rejected: it hides code behind a tooltip that is mostly a restatement
of the line the cursor is already on. Four characters left to the `when` keyword gets the same
answer without covering anything.

### Known-adjacent, not fixed here

A condition `condition::classify` has no label for renders as a verbatim echo of the line under
the cursor — `when: inventory_hostname == 'web01'` hovers as itself. That is a classifier
coverage gap, not an anchoring one, and it looks identical under every anchoring scheme tried.

## Done when

- [x] hovering a module name shows module provenance even when the task has a `when:` and inlay
      hints are on — `module_provenance_survives_a_when_with_hints_on` (uses `ping` from
      `demo/library/`, so it doesn't need an Ansible install to resolve into)
- [x] the `when:` explanation is reachable by hovering the `when:` clause, regardless of the
      inlay-hints setting — asserted under both `hints: true` and `hints: false`
- [x] hovering a variable inside `when:` either shows its definition/registration or is
      deliberately silent — shows the registration; already worked, now pinned
- [x] pinned on the `cmd_result` task in `demo/tasks/variables.yml` —
      `when_module_and_variable_each_own_their_token`
- [x] a conditional include/role/`import_playbook` still says it is guarded, at the reference —
      the regression the first attempt introduced, now `guard_line` appended to the reference
      hover rather than replacing it
