# T-178 — An inventory source's ansible_group_priority is indexed as a variable, and it is not one

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P3       | S    | —          |

## Symptom

`ini_vars` and `yaml_vars` return `ansible_group_priority` as a variable the inventory
defines. Ansible never defines it there. Run against the readers:

```
ini_vars  -> ["ansible_group_priority", "who"]
yaml_vars -> ["ansible_group_priority", "who"]
```

Measured on 2.21.2 (T-062 box 7, table on `vars::ignored_group_priority`), the same two
inventories resolve `{{ ansible_group_priority | default('ABSENT') }}` to **`ABSENT`** — the
key is consumed as merge order by `Group.set_variable` (`inventory/group.py:216-217`) and
never stored. So we index a name nothing can read.

Hover and go-to-definition are the visible surfaces: both point at the inventory line as the
definition of a variable that does not exist. `var-undefined` is unaffected either way —
every `ansible_*` name is already unconditionally exempt, so no diagnostic moves.

P3 rather than P1 despite being a lie: one rarely-written key, two answer surfaces, no false
squiggle. Same tier as [T-132] and [T-133], the other "hover points somewhere imprecise"
tickets.

## Cause

The readers collect every `name=value` in a `[group:vars]` section and every key under a
YAML group's `vars:`, with no exception list. That is right for every other key — this is
the one Ansible intercepts before it becomes a variable.

The inverse case is already handled, which is how this surfaced: T-062 box 7 warns that the
key in `group_vars/`/`host_vars/` is *inert as a control while remaining a real variable*.
This ticket is the other half — inert as a *variable* while being a real control. Both halves
are one fact about one key and should not be described in two places that can drift.

## Fix

Drop `ansible_group_priority` in `ini_vars`, `yaml_vars` and `toml_vars`, reusing the
`GROUP_PRIORITY` constant that box 7 added to `vars.rs` rather than respelling the name at
three more sites.

`toml_vars` is included on shape, not on measurement — it is the same `[group.vars]` table
feeding the same `Group.set_variable`, but it was not probed. Probe it before the change
lands rather than assuming the ini result carries over (rule 1).

Worth checking while here, since it decides whether the drop is unconditional: what an
**invalid** value does. Measured on 2.21.2, `ansible_group_priority={{ some_var }}` in an
inventory does not raise — `set_priority` catches the `ValueError`, warns
`Invalid priority value ... Setting priority to default value`, and the run proceeds at the
default. So the key is still consumed and still not a variable even when it is unusable, and
the drop does not need a validity test. Confirm that holds for a non-numeric literal too.

## Done when

- [ ] `ini_vars`, `yaml_vars` and `toml_vars` each drop the key, one test per reader
- [ ] a probe against real ansible pins that the key is absent from an inventory-defined
      host's vars, for all three formats, with a same-file ordinary variable as the control
      that must still be present
- [ ] hover and go-to-definition on the key report nothing from an inventory source,
      asserted per consumer
- [ ] the key is still indexed from `group_vars/`/`host_vars/`, where it genuinely is a
      variable — the T-062 box 7 control, re-asserted here so the two halves cannot drift
- [ ] `GROUP_PRIORITY` is the only spelling of the name in the codebase
- [ ] corpus count unchanged (the key appears zero times in `~/app`, so any move is a bug)

[T-132]: T-132-go-to-definition-on-a-module-with-an-action-plugin-twin-offe.md
[T-133]: T-133-notinworkspace-hover-lumps-three-different-situations-into-o.md
