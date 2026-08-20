# T-178 — An inventory source's ansible_group_priority is indexed as a variable, and it is not one

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P3       | S    | —          |

## Symptom

All three readers return `ansible_group_priority` as a variable the inventory defines, from
**both** positions it can be written in. Ansible defines it in only one of them. Run against
the readers, six fixtures, one per format per position:

```
reader/position | returns
----------------+---------------------------------------
ini   group     | ["ansible_group_priority", "control"]   <- wrong
ini   host      | ["ansible_group_priority", "control"]   <- correct
yaml  group     | ["ansible_group_priority", "control"]   <- wrong
yaml  host      | ["ansible_group_priority", "control"]   <- correct
toml  group     | ["ansible_group_priority", "control"]   <- wrong
toml  host      | ["ansible_group_priority", "control"]   <- correct
```

Against what Ansible actually defines for the same six files (below), that is:

| position written | ansible defines it | we return it | verdict              |
| ---------------- | ------------------ | ------------ | -------------------- |
| group `vars`     | **no**             | yes          | **wrong** — this bug |
| host entry       | **yes**, `10`      | yes          | correct today        |

Identical in ini, yaml and toml — the format is irrelevant, the position is everything. Three
of six cells are wrong.

Hover and go-to-definition are the visible surfaces: both point at the inventory line as the
definition of a variable that does not exist. `var-undefined` is unaffected either way —
every `ansible_*` name is already unconditionally exempt, so no diagnostic moves.

P3 rather than P1 despite being a lie: one rarely-written key, two answer surfaces, no false
squiggle. Same tier as [T-132] and [T-133], the other "hover points somewhere imprecise"
tickets.

## Cause

**The position decides, not the format.** Measured on core 2.21.3 with
`ansible-inventory -i <file> --host node1`, which prints what the inventory contributes with
no templating in the way — every file carries an ordinary `control` variable in the *same*
position, so an absence is an absence and not a fixture that was never read:

```
ini_group.ini    { "control": "from_group_vars" }
ini_host.ini     { "ansible_group_priority": 10, "control": "from_host_line" }
yaml_group.yml   { "control": "from_group_vars" }
yaml_host.yml    { "ansible_group_priority": 10, "control": "from_host_entry" }
toml_group.toml  { "control": "from_group_vars" }
toml_host.toml   { "ansible_group_priority": 10, "control": "from_host_table" }
```

The fixtures, so this is re-runnable:

```ini
# ini_group.ini                     # ini_host.ini
[web]                               [web]
node1                               node1 ansible_group_priority=10 control=from_host_line

[web:vars]
ansible_group_priority=10
control=from_group_vars
```
```yaml
# yaml_group.yml                    # yaml_host.yml
web:                                web:
  hosts:                              hosts:
    node1:                              node1:
  vars:                                   ansible_group_priority: 10
    ansible_group_priority: 10            control: from_host_entry
    control: from_group_vars
```
```toml
# toml_group.toml                   # toml_host.toml
[web.vars]                          [web.hosts.node1]
ansible_group_priority = 10         ansible_group_priority = 10
control = "from_group_vars"         control = "from_host_table"
[web.hosts.node1]
```

In a group position the key is consumed as merge order by `Group.set_variable`
(`inventory/group.py:216-217`) and never stored. `Host.set_variable` (`inventory/host.py:119-130`)
has no such branch, so in a host position it is stored like any other variable — and is
genuinely readable.

Which makes a host-entry priority the mirror image: **inert as a control, real as a
variable.** Measured with T-062 box 7's two-group merge shape — `alpha` and `zulu` both define
`who`, `node1` in both — with the group position as the control that must come out different:

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
takes the host-position key with it, and the six-row table above shows that is the half we
currently get *right*. It would have traded "hover points at a non-variable" for "hover says a
real variable is never defined", which is the worse of the two.

The probe that missed it is the rule-2 shape: a `[group:vars]` fixture has no host-position
candidate in it, so it could not have produced the other answer.

Drop the key only where the reader is walking a **group vars** position. The shared collectors
serve both positions, so the filter goes at the group-vars *caller* — putting it inside the
helper is the blanket drop again, one level down:

| reader      | gate here                                     | must stay untouched                |
| ----------- | --------------------------------------------- | ---------------------------------- |
| `ini_vars`  | `inventory.rs:620`, the `Section::Vars` branch | the host-line loop below it        |
| `yaml_vars` | `inventory.rs:532`, `Some("vars") => bindings` | `bindings` itself, and `hosts:` at 536 |
| `toml_vars` | `inventory.rs:310`, `collect_toml(vars, ..)`   | `collect_toml` itself, and hosts at 315 |

`GROUP_PRIORITY` (`vars.rs:1432`, private) is the existing spelling; move it to `inventory.rs`
as `pub(crate)` and have `ignored_group_priority` read it from there, rather than respelling
the name at three more sites. The measured tables above belong in a doc comment at the drop
site, the way box 7's table sits on `ignored_group_priority` — there is no harness that runs
real ansible from a test, so the comment is the only thing that keeps the measurement next to
the code it justifies. The table on `vars.rs:1445` carries only two of the three positions and
needs the host-entry row, or the two will drift.

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
- [ ] the measured tables above sit in a doc comment at the drop site, and `vars.rs:1445`
      gains the host-entry row
- [ ] hover and go-to-definition report nothing for the key in a group position, and still
      answer for it in a host position, asserted per consumer
- [ ] the key is still indexed from `group_vars/`/`host_vars/`, where it genuinely is a
      variable — the T-062 box 7 control, re-asserted here so the two halves cannot drift
- [ ] `GROUP_PRIORITY` is the only spelling of the name in the codebase
- [ ] seen red before the fix, per rule 5
- [ ] corpus count unchanged (the key appears zero times in `~/app`, so any move is a bug)

[T-132]: T-132-go-to-definition-on-a-module-with-an-action-plugin-twin-offe.md
[T-133]: T-133-notinworkspace-hover-lumps-three-different-situations-into-o.md
