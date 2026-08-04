# T-078 — `when:` explanation and module provenance fight over the module-name token

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P3       | S    | —          |

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

## Done when

- [ ] hovering a module name shows module provenance even when the task has a `when:` and inlay
      hints are on
- [ ] the `when:` explanation is reachable by hovering the `when:` clause, regardless of the
      inlay-hints setting
- [ ] hovering a variable inside `when:` either shows its definition/registration or is
      deliberately silent — but is consistent with go-to-definition, not an accident of which
      reference kinds `extract` returns
- [ ] pinned on the `cmd_result` task in `demo/tasks/variables.yml`
