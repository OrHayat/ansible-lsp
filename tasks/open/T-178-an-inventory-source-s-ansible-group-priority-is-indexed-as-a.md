# T-178 — An inventory source's ansible_group_priority is indexed as a variable, and it is not one

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P3       | S    | —          |

## Symptom

All three readers return `ansible_group_priority` as a variable the inventory defines, from
**every** position it can be written in. Ansible defines it in only some of them. Run against
the readers over thirteen fixtures — every position the three readers walk, enumerated from
their own branches, not from a guess about which ones matter:

| #  | fixture                       | ansible  | us before | verdict   |
| -- | ----------------------------- | -------- | --------- | --------- |
| 01 | ini `[web:vars]`              | `ABSENT` | `10`      | **wrong** |
| 02 | ini host line                 | `10`     | `10`      | ok        |
| 03 | ini ungrouped host line       | `10`     | `10`      | ok        |
| 04 | ini `[all:vars]`              | `ABSENT` | `10`      | **wrong** |
| 05 | ini `[parent:vars]`           | `ABSENT` | `10`      | **wrong** |
| 06 | yaml group `vars:`            | `ABSENT` | `10`      | **wrong** |
| 07 | yaml host entry               | `10`     | `10`      | ok        |
| 08 | yaml child group `vars:`      | `ABSENT` | `10`      | **wrong** |
| 09 | yaml child host entry         | `10`     | `10`      | ok        |
| 10 | yaml `all:` `vars:`           | `ABSENT` | `10`      | **wrong** |
| 11 | toml `[web.vars]`             | `ABSENT` | `10`      | **wrong** |
| 12 | toml `[web.hosts.node1]`      | `10`     | `10`      | ok        |
| 13 | toml `[all.vars]`             | `ABSENT` | `10`      | **wrong** |

**Eight of thirteen**, not the three of six a first six-fixture probe suggested. `all:vars`,
a parent group's vars and a child group's vars are broken too, and none of them appeared in
that first probe — the count was wrong until the position list came from the readers' own
branches. The rule does hold across all thirteen: every group position is consumed, every
host position is stored, in all three formats.

### Which surface actually shows it

This ticket twice claimed, without running it, that hover and go-to-definition are the visible
surfaces. **They are not.** Measured against the six consumers of `cached_definitions`, with
the key in a **host** position — where it genuinely is a variable — and an ordinary `control`
variable on the same line as the control that must come out different:

| consumer                        | answers for `ansible_group_priority` | for `control` |
| ------------------------------- | ------------------------------------ | ------------- |
| `variable_hover_at`             | **no** — `<none>`                    | yes, `= from_host_line` |
| `definition_at`                 | **no** — no jump                     | yes, `inv.ini` line 1 |
| condition hover (`hover_at`)    | **no** — `<none>`                    | n/a |
| `path_substitution_hover`       | **YES**                              | n/a |
| `variable_coverage_diagnostics` | exempt by design, every `ansible_*`  | n/a |
| `resolved_references`           | not measured — needs a `Client`      | n/a |

The index carries the key in every case (`INDEX -> ["ansible_group_priority", "control"]`), so
this is the consumers filtering, not the index withholding.

**Why the first two are silent:** `is_injected` returns true for *any* name starting with
`ansible_` (`condition.rs:476`). `variable_hover_at` short-circuits on it at `main.rs:1678`
before reading the index, and `variable_defs_at` uses `vars::uses`, which drops injected names
outright. No `ansible_*` name can reach either surface from any position, before or after this
fix.

**The surface that does show it** is the templated-path hover. Given
`vars_files: - "vars/{{ ansible_group_priority }}.yml"`:

```
→ vars/10.yml
Substituting:
- `ansible_group_priority` = `10` — inventory · inv.ini:2
```

Correct from a host position. From a **group** position that is the lie this ticket is about: a
confident value, sourced to an inventory line, for something ansible never defined — the exact
shape the project's opening rule names.

`var-undefined` is unaffected either way, every `ansible_*` name being unconditionally exempt,
so no diagnostic moves.

P3 rather than P1: one rarely-written key, one answer surface, and that surface needs the key
used inside a templated path. Same tier as [T-132] and [T-133].

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

## What the source says, and the one case measurement missed

Reading the plugins after the fix landed turned up a case no fixture covered. All three
static plugins set **group** vars directly (`ini.py:234`, `yaml.py:155`, `toml.py:120`) but
route **host** vars through `_populate_host_vars` (`plugins/inventory/__init__.py:223-231`),
which calls `InventoryData.set_variable` — and that dispatches on the *name*, not the
position (`inventory/data.py:233-245`):

```python
if entity in self.groups:      # <- tested FIRST
    inv_object = self.groups[entity]
elif entity in self.hosts:
    inv_object = self.hosts[entity]
```

So a host sharing a name with a group has its host-line vars applied to the **Group**, which
consumes the key. Measured on 2.21.3, with a non-colliding host in the same file as control:

| inventory                                      | host  | priority defined |
| ---------------------------------------------- | ----- | ---------------- |
| `[web]` + host `web` (ini, yaml and toml alike) | `web` | **no** — consumed |
| `[web]` + host `node1` (control)                | `node1` | yes, `10`      |

Ansible warns `Found both group and host with same name: web` and carries on. **We still
report the key there**, so the rule as shipped is "group position → not a variable", while
the true rule is "whatever the entity name resolves to, groups first".

Two other things the source settles, both negative: `Group.set_variable` intercepts exactly
one key — a single `if key == 'ansible_group_priority'` — so no other name is consumed this
way; and `_parse_host_definition` (`ini.py:299-330`) puts every `k=v` into the vars dict
untouched, the port coming from the hostname token rather than a pair, so nothing is quietly
removed from host-line vars either.

Closing the collision case needs the inventory's set of group names, which no reader has —
they deliberately return hosts and not group names, and a directory source spreads groups
across files, so it cannot be answered per file. Recorded as a limit on `GroupPosition` in
`inventory.rs` rather than left to be rediscovered.

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
Re-measured on 2.21.3, both shapes an invalid value can take:

| written                                    | result                                       |
| ------------------------------------------ | -------------------------------------------- |
| `ansible_group_priority=not_a_number`      | warns, default priority, key still consumed  |
| `ansible_group_priority={{ undefined_x }}` | warns, default priority, key still consumed  |

`set_priority` catches the `ValueError`, warns `Invalid priority value ... Setting priority to
default value`, and the run proceeds. The key is consumed even when it is unusable, so the
drop needs no validity test. (The non-numeric literal was an open question in this ticket
until now; both shapes are settled.)

Out of scope, but the second row raises it fairly: a host-entry priority is an inert control,
the same finding box 7 reports for `group_vars/`. `ignored_group_priority` gates on
`under_vars_plugin_dir` and so cannot see an inventory source. File separately if wanted —
this ticket is the false definition, not the missing hint.

## Why the per-consumer test is writable

An earlier revision of the per-consumer box said the assertion needed "an LSP
workspace-and-settings harness that does not exist yet", and that "the existing hover helpers
only cover injected vars". Both are false. The note was written from the shape of the call
graph rather than from reading it, which is rule 1 applied to our own code instead of
ansible's.

What is true is the part that makes it *look* blocked. Both consumers reach the index through
`cached_definitions` (`main.rs:194`), which takes its inventory paths from `inventory_setting()`
— two process-global mutexes, `INVENTORY_SETTING` and `WORKSPACE_ROOT` (`main.rs:218-220`). A
test cannot set those: they are process-wide and the suite runs in parallel threads in one
binary. Every inventory test in the tree sidesteps them by building
`ScanCache::default().with_inventory(..)` and calling core directly — which is exactly why
none of them reaches hover or go-to-definition, and why the surface looked untestable.

The way through is that `with_inventory` sets `inventory_override` (`cache.rs:411`), and an
**empty** override falls through to `cfg.inventory` — the `inventory =` key of an
`ansible.cfg`. So a fixture directory carrying its own `ansible.cfg` drives the real consumers
with no global touched. The calling pattern already exists and is not injected-vars-only:
`variable_hover_at` and `definition_at` are called directly against fixture paths throughout
`main.rs:4758-5170`. T-171's Cmd+click assertion at `main.rs:5005` is the model — it is
deliberately routed through `definition_at` rather than the helper, with a comment recording
that calling the helper is what let a missing paint ship. Same argument, same shape.

### Why not `MemFs`

The obvious question, since `ansible_core::testing::MemFs` exists and would need no disk. Two
reasons, and the second is the one that matters.

`testing.rs:27` states the rule the crate already follows: prefer `MemFs` where the code under
test takes a `&dyn Fs`, and `tree` for the paths that **hardcode `StdFs`**. `cached_definitions`
is the second kind — it builds `ScanCache::default()` internally, and the consumers hand it no
cache. Injection only happens at `ScanCache::new(fs)` (`cache.rs:272`), one level below where
these two calls sit.

But even threading a `ScanCache` down would not work, because the LSP crate reads disk outside
the `Fs` seam entirely — `main.rs:2711` (`located_at`, turning a byte span into a line/column),
and `1609`, `1710`, `1727` (hover rendering a definition's value). `Fs` is ansible-core's door
by design (`fs.rs:1`); this crate never adopted it. A `MemFs` fixture would therefore make
go-to-definition return `None` and hover render blanks — and **silently**, via `.ok()?` and
`unwrap_or_default()` at those sites. The negative half of this box would go green for entirely
the wrong reason: the rule-2 trap again, and harder to spot than the `ANSIBLE_CONFIG` one.

That disk-only property is a bug in its own right, measured and filed as [T-199] — hover
reports the saved value and go-to-definition returns a saved-file line while the editor draws
an unsaved buffer. Its fix threads `State` into these same two consumers, which is the seam
this box wants; whoever takes either one should read the other first.

One build note for whoever writes the test: `ansible_core::testing` is gated behind
`feature = "test-fixtures"` (`ansible-core/Cargo.toml:18`), which only ansible-core enables for
itself. Using `project()` from here needs a dev-dependency added to
`crates/ansible-lsp/Cargo.toml`:

```toml
[dev-dependencies]
ansible-core = { path = "../ansible-core", features = ["test-fixtures"] }
```

One hazard to clear when writing it, not a blocker: `cached_definitions` builds
`ScanCache::default()` **without** `with_env(EnvMap::empty())`, so an ambient `ANSIBLE_CONFIG`
in the invoking shell can replace the fixture's cfg — `cache.rs:299-302` exists for exactly
this. A test that passes because the cfg was never read is the rule-2 shape: it must be seen
red in both directions first — with the group gate removed, and with the fixture's inventory
made unreachable.

## Done when

- [x] `ini_vars`, `yaml_vars` and `toml_vars` each drop the key from a **group vars**
      position, one test per reader — covering `all`, a parent and a child group, since one
      gate per reader turned out to cover every depth
- [x] each reader still returns it from a **host** position, one test per reader — this is
      the control, and the fix this ticket originally proposed fails it
- [x] the measured tables above sit in a doc comment at the drop site, and `vars.rs` gained
      the host-entry row
- [x] `path_substitution_hover` substitutes the key from a **host** position and refuses to
      from a **group** position, asserted at that consumer —
      `group_priority_substitutes_a_path_from_a_host_position_and_never_from_a_group`. Two
      earlier revisions of this box named hover and go-to-definition instead; both are silent
      for every `ansible_*` name, so as written it asked for an assertion that cannot hold.
      Seen red three ways, per rule 5: with the group gate removed the group case substitutes
      and fails; with the reader-wide drop this ticket originally prescribed the host case
      stops substituting and fails; and with the fixture's `ansible.cfg` pointed at a missing
      inventory the `control` assertion fails, which is what keeps the negative from passing
      on an inventory that was never read.
- [x] the key is still indexed from `group_vars/`/`host_vars/`, where it genuinely is a
      variable — the T-062 box 7 control, still green at `vars.rs`
- [x] `GROUP_PRIORITY` is the only spelling of the name in the codebase — moved to
      `inventory.rs` as `pub(crate)`, `vars.rs` reads it from there
- [x] seen red before the fix, per rule 5 — and red in **both** directions: with no gate the
      wiring test reports 2 definitions instead of 1, and with the reader-wide drop this
      ticket originally prescribed it reports 0. The first break attempt was not faithful
      (flipping `GroupPosition::consumes` leaves ini host lines untouched, because that path
      never calls it) and passed; the honest simulation is a `retain` over the reader's whole
      output, and that one fails both the wiring test and the ini control.
- [x] corpus count unchanged — the key appears zero times in `~/app/ansible`, so nothing
      could move
- [ ] a host sharing a name with a group has the key consumed, and we still report it — see
      the section above. Needs a group-name set assembled across the whole inventory source,
      so it is not a reader-local change. Fix here or split out, but not silently.

[T-132]: T-132-go-to-definition-on-a-module-with-an-action-plugin-twin-offe.md
[T-133]: T-133-notinworkspace-hover-lumps-three-different-situations-into-o.md
[T-199]: T-199-hover-and-go-to-definition-read-the-saved-file-so-an-unsaved.md
