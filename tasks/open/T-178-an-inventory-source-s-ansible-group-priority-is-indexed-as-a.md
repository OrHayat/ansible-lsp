# T-178 — An inventory source's ansible_group_priority is indexed as a variable, and it is not one

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P3       | S    | —          |

## Symptom

`ini_vars` and `yaml_vars` return `ansible_group_priority` as a variable the inventory
defines. Written in a **group vars** position, Ansible never defines it. Run against the
readers:

```
ini_vars  -> ["ansible_group_priority", "who"]
yaml_vars -> ["ansible_group_priority", "who"]
```

Hover and go-to-definition are the visible surfaces: both point at the inventory line as the
definition of a variable that does not exist. `var-undefined` is unaffected either way —
every `ansible_*` name is already unconditionally exempt, so no diagnostic moves.

P3 rather than P1 despite being a lie: one rarely-written key, two answer surfaces, no false
squiggle. Same tier as [T-132] and [T-133], the other "hover points somewhere imprecise"
tickets.

## Cause

**The position decides, not the format.** Measured on 2.21.3 across all three readers and
both positions, each inventory carrying an ordinary variable in the *same* position as the
control — without it `ABSENT` would only prove the probe read nothing:

| position written | ini      | yaml     | toml     | control |
| ---------------- | -------- | -------- | -------- | ------- |
| group `vars`     | `ABSENT` | `ABSENT` | `ABSENT` | present |
| host entry       | **`10`** | **`10`** | **`10`** | present |

In a group position the key is consumed as merge order by `Group.set_variable`
(`inventory/group.py:216-217`) and never stored. `Host.set_variable` (`inventory/host.py:119-130`)
has no such branch, so in a host position it is stored like any other variable — and is
genuinely readable.

Which makes a host-entry priority the mirror image: **inert as a control, real as a
variable.** Measured with T-062 box 7's two-group merge shape, the group position acting as
the control that must come out different:

| `ansible_group_priority=10` written in | merge winner           |
| -------------------------------------- | ---------------------- |
| nowhere (baseline)                     | `zulu`                 |
| `[alpha:vars]` — group position        | **`alpha`** — honoured |
| `node1`'s host line — host position    | `zulu` — **ignored**   |

So there are three positions for one key, and only the first is this ticket's bug:

| position                        | works as a control | is a variable      |
| ------------------------------- | ------------------ | ------------------ |
| inventory group `vars`          | yes                | **no** — the bug   |
| inventory host entry            | no                 | yes                |
| `group_vars/`/`host_vars/` file | no                 | yes                |

The third row is T-062 box 7, already handled by `vars::ignored_group_priority`. The second
row was unknown when this ticket was written, and it is what breaks the fix it originally
proposed.

## Fix

**This ticket's original fix was wrong and would have made the tool lie worse.** It said to
drop the key in `ini_vars`, `yaml_vars` and `toml_vars`, describing them as reading only
`[group:vars]` sections and YAML `vars:` mappings. They do not — all three also collect host
vars: ini host lines, `hosts:` entries, and `[group.hosts.name]` tables. A reader-wide drop
takes the host-position key with it, and that one really is a variable. It would have traded
"hover points at a non-variable" for "hover says a real variable is never defined", which is
the worse of the two.

The probe that missed it is the rule-2 shape: a `[group:vars]` fixture has no host-position
candidate in it, so it could not have produced the other answer.

Drop the key only where the reader is walking a **group vars** position:

- `ini_vars` — the `Section::Vars` branch only, never the host-line branch
- `yaml_vars` — `bindings` reached from the `vars` key only, not from `hosts`
- `toml_vars` — the `vars` table only, not the `hosts` tables

`GROUP_PRIORITY` (`vars.rs`) is the existing spelling; move it to `inventory.rs` or make it
`pub(crate)` rather than respelling the name at three more sites. The measured tables above
belong in a doc comment at the drop site, the way box 7's table sits on
`ignored_group_priority` — there is no harness that runs real ansible from a test, so the
comment is the only thing that keeps the measurement next to the code it justifies.

The drop is unconditional in that position — an **invalid** value does not change it.
Measured on 2.21.2: `ansible_group_priority={{ some_var }}` in an inventory does not raise;
`set_priority` catches the `ValueError`, warns `Invalid priority value ... Setting priority to
default value`, and the run proceeds at the default. So the key is still consumed and still
not a variable even when it is unusable, and no validity test is needed.

Out of scope, but the second row raises it fairly: a host-entry priority is an inert control,
the same finding box 7 reports for `group_vars/`. `ignored_group_priority` gates on
`under_vars_plugin_dir` and so cannot see an inventory source. File separately if wanted —
this ticket is the false definition, not the missing hint.

## Done when

- [ ] `ini_vars`, `yaml_vars` and `toml_vars` each drop the key from a **group vars**
      position, one test per reader
- [ ] each reader still returns it from a **host** position, one test per reader — this is
      the control, and the fix this ticket originally proposed fails it
- [ ] the measured tables above sit in a doc comment at the drop site
- [ ] hover and go-to-definition report nothing for the key in a group position, and still
      answer for it in a host position, asserted per consumer
- [ ] the key is still indexed from `group_vars/`/`host_vars/`, where it genuinely is a
      variable — the T-062 box 7 control, re-asserted here so the two halves cannot drift
- [ ] `GROUP_PRIORITY` is the only spelling of the name in the codebase
- [ ] seen red before the fix, per rule 5
- [ ] corpus count unchanged (the key appears zero times in `~/app`, so any move is a bug)

[T-132]: T-132-go-to-definition-on-a-module-with-an-action-plugin-twin-offe.md
[T-133]: T-133-notinworkspace-hover-lumps-three-different-situations-into-o.md
