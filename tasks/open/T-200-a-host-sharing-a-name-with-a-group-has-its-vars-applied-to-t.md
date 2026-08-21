# T-200 — A host sharing a name with a group has its vars applied to the group, and we report them as the host's

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P3       | M    | —          |

## Symptom

Split out of [T-178], which fixed the neighbouring rule and left this one measured but not
done. T-178's rule is "a group `vars` position does not define `ansible_group_priority`". The
true rule is wider: **whatever the entity name resolves to decides, and groups are checked
first** — so a *host* position can be a group position too, when the host's name is also a
group's.

Measured on core 2.21.3, with a non-colliding host in the same file as the control:

| inventory                                       | host    | priority defined  |
| ----------------------------------------------- | ------- | ----------------- |
| `[web]` + host `web` (ini, yaml and toml alike) | `web`   | **no** — consumed |
| `[web]` + host `node1` (control)                | `node1` | yes, `10`         |

Ansible warns `Found both group and host with same name: web` and carries on. **We still
report the key there**, so hover's templated-path substitution will name an inventory line as
the source of a value ansible never defined — the same lie T-178 fixed for the group position,
reached by a different route.

P3 for the same reasons as T-178: one rarely-written key, one answer surface
(`path_substitution_hover` — see T-178's "Which surface actually shows it" for why hover and
go-to-definition are silent for every `ansible_*` name), and a name collision is itself rare
enough that ansible only warns about it.

## Cause

All three static plugins set **group** vars directly (`ini.py:234`, `yaml.py:155`,
`toml.py:120`) but route **host** vars through `_populate_host_vars`
(`plugins/inventory/__init__.py:223-231`), which calls `InventoryData.set_variable` — and that
dispatches on the *name*, not the position (`inventory/data.py:233-245`):

```python
if entity in self.groups:      # <- tested FIRST
    inv_object = self.groups[entity]
elif entity in self.hosts:
    inv_object = self.hosts[entity]
```

A `Group` consumes the key (`inventory/group.py:216-217`); a `Host` stores it
(`inventory/host.py:119-130`). So the host-position path lands on a Group whenever the names
collide, and the key is eaten.

Our readers cannot see this. `ini_vars`, `yaml_vars` and `toml_vars` each take **one file**
and return `Vec<InventoryVar>`; they deliberately return hosts and not group names, and a
directory source spreads groups across files, so no reader can answer "is this name also a
group?" on its own. `GroupPosition` (`inventory.rs:118`) carries a comment recording this
limit.

## Fix

The loop that makes it tractable already exists — `vars.rs:1220` walks every source in the
resolved inventory before any of them is read:

```rust
for src in crate::inventory::sources(&ctx.config, walk.cache) {
    read_inventory(&src, out, walk);
}
```

So: a first pass collecting group names across **all** sources, then the existing read with
that set in hand.

| piece                                              | notes |
| -------------------------------------------------- | ----- |
| `ini_groups` / `yaml_groups` / `toml_groups`        | mirror the existing `*_hosts` functions |
| the three readers take the group-name set           | the `GroupPosition::No` branch upgrades to `Yes` when the host's name is in it — the name is already in hand at that point |
| ~20 positional call sites in `inventory.rs`'s tests | the signature change reaches all of them |
| two-pass at `vars.rs:1220`                          | small |

Sized **M** for the signature change and its call sites, which is why it is not in T-178
(an S).

Worth deciding while doing it: ansible *warns* on the collision. A tool that knows the two
names collide could say so, which is more useful than silently declining to report one
variable. Out of scope unless it falls out for free.

## Done when

- [ ] a host whose name matches a group has its host-position `ansible_group_priority`
      dropped, one test per reader, with a **non-colliding host in the same fixture** as the
      control that must still report it — the shape the measurement above used
- [ ] group names are collected across every file of a **directory** source, not just the one
      being read, asserted with the collision and the definition in different files — this is
      the case a per-file reader cannot get right and the whole reason for the two-pass
- [ ] an ordinary variable on the same colliding host line is **still** reported, since only
      this one key is consumed — `Group.set_variable` intercepts exactly one name
- [ ] `path_substitution_hover` refuses to substitute from a colliding host position, per
      T-178's per-consumer box and by the same fixture shape
- [ ] seen red before the fix, per rule 5, and the break confirmed to have landed in the file
      before believing a green run
- [ ] the `GroupPosition` comment at `inventory.rs:115-116` recording this as an open limit is
      removed, since it will no longer be one

[T-178]: T-178-an-inventory-source-s-ansible-group-priority-is-indexed-as-a.md
