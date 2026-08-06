# Upstream issues to file against ansible/ansible — inventory parses that fail quietly

Not filed yet. Read from ansible-core **2.22.0.dev0** (`lib/ansible/release.py:20`).

Two independent findings that share a theme: an inventory mistake produces a *working-looking*
inventory rather than an error.

---

## Issue 1 — a YAML inventory that fails to parse is silently re-parsed by the INI plugin

**Component:** `lib/ansible/inventory/manager.py`, `lib/ansible/plugins/inventory/ini.py`

**Summary.** `INVENTORY_ENABLED` defaults to
`['host_list', 'script', 'auto', 'yaml', 'ini', 'toml']` (`config/base.yml:1726-1733`), and
`parse_source` tries each in order, accepting the first whose `verify_file` passes and whose
`parse` succeeds (`inventory/manager.py:286-312`).

`ini.verify_file` accepts **any readable file that is not `.toml`**
(`plugins/inventory/ini.py:108-110`):

```python
def verify_file(self, path):
    return super(InventoryModule, self).verify_file(path) and os.path.splitext(path)[1] != '.toml'
```

So when a `.yml` inventory has a YAML syntax error, the `yaml` plugin raises, the loop
continues, and the `ini` plugin happily parses the same file as INI — where almost any text is
a valid host line. The YAML error is then discarded, because failures are only surfaced if
*nothing* parsed (`inventory/manager.py:335`):

```python
# only if no plugin processed files should we show errors.
if not parsed:
    ...
```

The result is a populated inventory of nonsense host names, and an exit status of 0.

**Reproduction.**

```yaml
# inv.yml — note the bad indentation on 'hosts'
all:
  children:
    web:
    hosts:
        web1:
```

```
$ ansible-inventory -i inv.yml --list
```

Observed: no error. The inventory contains hosts named after fragments of the YAML — e.g.
`all:`, `children:`, `web:` — because the INI plugin read them as host lines. The actual YAML
error is visible only at `-vvv` (`inventory/manager.py:311`).

Expected: the YAML failure is reported, or at minimum a warning says the file was parsed by a
plugin other than the one its extension implies.

**Suggested fix.** Either narrow `ini.verify_file` to extensions it should own, or — less
disruptive — warn when a file with a `.yml`/`.yaml` extension is parsed by a plugin other than
`yaml`, and retain the highest-priority plugin's exception for display even when a later
plugin succeeds.

Note `INVENTORY_UNPARSED_IS_FAILED` and `INVENTORY_ANY_UNPARSED_IS_FAILED` both default
`False` (`config/base.yml:1760-1770`, `:1714-1725`), and neither helps here — from Ansible's
point of view the source *was* parsed.

---

## Issue 2 — `ansible_group_priority` set in `group_vars/` is silently ignored

**Component:** `lib/ansible/inventory/group.py`, `lib/ansible/inventory/manager.py`

**Summary.** `ansible_group_priority` controls merge order between groups at the same depth
(`inventory/helpers.py:25-26`):

```python
sorted(groups, key=lambda g: (g.depth, g.priority, g.name))
```

It is consumed — never stored as an ordinary variable — inside `Group.set_variable`
(`inventory/group.py:216-217`):

```python
if key == 'ansible_group_priority':
    self.set_priority(int(value))
```

That method is reached only from inventory-source parsing (`inventory/data.py:233-245`).
Variables contributed by **vars plugins**, which is how `group_vars/` files are loaded
(`plugins/vars/host_group_vars.py`), are merged later and bypass it entirely
(`inventory/manager.py:248-249`). So:

```yaml
# group_vars/web.yml
ansible_group_priority: 10     # does nothing
```

has no effect, while the same key in an inventory file does. Nothing warns, and the variable
remains visible in `hostvars`, so it looks like it was accepted.

**Reproduction.** Two same-depth groups defining the same variable, with
`ansible_group_priority` in `group_vars/` intended to break the tie — the alphabetically later
group wins regardless of the priority set.

**Expected.** Either honour it wherever it is set, or warn that it is ignored outside an
inventory source. Documentation
(`docs/.../intro_inventory.rst`, "How variables are merged") does not mention the restriction.

**Also note** it is not templated: `int(priority)` runs at parse time
(`inventory/group.py:253-262`), so `ansible_group_priority: "{{ x }}"` raises `ValueError`
rather than resolving. And `depth` dominates priority in the sort key, which surprises people
who expect priority to be absolute.

---

## Our side

`T-062` (index ini inventories and extension-less `group_vars`/`host_vars`) carries the
editor-side diagnostics for both: warn that a `.yml` inventory will not parse as YAML before
Ansible silently reinterprets it, and warn that `ansible_group_priority` in `group_vars/` is a
no-op. Neither depends on these being fixed upstream.
