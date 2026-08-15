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
    # hardcode exclusion for TOML to prevent partial parsing of things we know we don't want
    return super().verify_file(path) and os.path.splitext(path)[1] != '.toml'
```

That comment (added in the 2.19 Data Tagging overhaul, `35750ed321`; before it the ini
plugin had no `verify_file` override at all) is upstream acknowledging the plugin does
"partial parsing of things we know we don't want" — and excluding only TOML. The suggested
fix below is the same move for `.yml`/`.yaml`.

So when a `.yml` inventory fails YAML parsing, the `yaml` plugin raises, the loop
continues, and the `ini` plugin parses the same file as INI. The YAML error is then
discarded, because failures are only surfaced if *nothing* parsed (`inventory/manager.py:335`):

```python
# only if no plugin processed files should we show errors.
if not parsed:
    ...
```

The result is a populated inventory of nonsense host names, and an exit status of 0.

The window is narrower than "any broken YAML": the ini plugin rejects a host line whose
first token ends in `:` ("ending in ':' is not allowed, this character is reserved to
provide a port", via `_expand_hostpattern`), so a typical block-YAML file full of bare
`all:` / `hosts:` lines fails **both** plugins — which does produce warnings (though still
exit 0). The silent fallback fires when YAML parsing fails **and** the lines happen to be
INI-acceptable, which is exactly the shape of realistic typos.

**Reproduction (measured, 2.21.2).**

```yaml
# typo.yml — meant to be a 'webservers' group with two hosts; the colons were forgotten,
# so as YAML this is scalars, not a mapping
webservers
  web1 ansible_host=10.0.0.5
  web2
```

```
$ ansible-inventory -i typo.yml --list
```

Observed: exit 0, **zero bytes of stderr**, and three *hosts* in `ungrouped` —
`webservers`, `web1`, `web2`. The intended group is now a host Ansible will try to reach,
and `web1`/`web2` are in no group, so their `group_vars` never apply. The YAML error is
visible only at `-vvv` (`inventory/manager.py:311`).

The same mechanism also accepts a well-formed INI inventory named `inv.yml` — it parses
completely and silently via the ini plugin, so the extension lies about which parser owns
the file (and about the value semantics: `ast.literal_eval` on host lines vs YAML types).

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
