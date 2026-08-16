# Upstream issue to file against ansible/ansible — two of `add_host`'s documented aliases also become host variables

Not filed yet. Measured on ansible-core **2.21.2** (`lib/ansible/release.py:20`); the code
below is long-standing and not a recent regression.

## Issue 1 — `host:` and `group:` are consumed as parameters *and* left behind as host variables

**Component:** `lib/ansible/plugins/action/add_host.py`

**Summary.** `add_host` documents six spellings for its two options
(`lib/ansible/modules/add_host.py:19-31`):

```yaml
name:
  aliases: [ host, hostname ]
groups:
  aliases: [ group, groupname ]
```

The action plugin honours all six when picking the host name and the group list
(`action/add_host.py:52`, `:68`):

```python
new_name = args.get('name', args.get('hostname', args.get('host', None)))
...
groups = args.get('groupname', args.get('groups', args.get('group', '')))
```

Then it turns every *remaining* key into a host variable, excluding a hardcoded set of four
(`:85-88`):

```python
host_vars = dict()
special_args = frozenset(('name', 'hostname', 'groupname', 'groups'))
for k in args.keys():
    if k not in special_args:
        host_vars[k] = args[k]
```

Six documented spellings, four excluded. `host` and `group` are missing from `special_args`,
so each is read as an alias *and* survives into `host_vars` under its own name. The host is
created correctly and quietly acquires a variable the author never wrote.

**Measured.** One host per spelling, each diffed against a `name:`-only baseline so a form
that contributed nothing could not be mistaken for one that did:

| written       | creates the host / group | also defines a variable |
| ------------- | ------------------------ | ----------------------- |
| `name:`       | yes                      | no                      |
| `hostname:`   | yes                      | no                      |
| `groups:`     | yes                      | no                      |
| `groupname:`  | yes                      | no                      |
| `host:`       | yes                      | **yes — `host`**        |
| `group:`      | yes                      | **yes — `group`**       |

All six created their host and group, so the aliases genuinely work; only the last two leak.

**Reproduction:**

```yaml
# repro.yml
- hosts: localhost
  gather_facts: false
  tasks:
    - ansible.builtin.add_host: {name: base,   ansible_connection: local}
    - ansible.builtin.add_host: {host: v_host, ansible_connection: local}
    - ansible.builtin.add_host: {name: v_group, group: g1, ansible_connection: local}

    - ansible.builtin.debug:
        msg: "{{ item }} EXTRA={{ hostvars[item].keys() | difference(hostvars['base'].keys() | list) | sort | join(',') }}"
      loop: [v_host, v_group]
```

```
"msg": "v_host EXTRA=host"
"msg": "v_group EXTRA=group"
```

The same run with `hostname:`/`groups:`/`groupname:` reports `EXTRA=` — nothing.

**Why this matters.** The docs present the six spellings as interchangeable, so choosing
`host:` over `name:` is presented as pure style. It is not: it silently adds a variable to
every host the task creates. `host` and `group` are both plausible names for a user's own
variable, so the collision is not hypothetical — a later `{{ group }}` reads the alias value
rather than the one the author defined, at precedence level 8, beating playbook `group_vars`.

It also makes the two halves of one task inconsistent with each other. In

```yaml
- add_host:
    name: web01
    host: web02
```

`name:` wins the host name and `host:` becomes a variable — so the same key is a parameter
for the purpose of the alias lookup and a variable for the purpose of `host_vars`, in one
task, decided by which other keys happen to be present.

**Expected.** A documented alias should behave exactly as the option it aliases. Either
`special_args` covers all six spellings, or the docs stop calling `host`/`group` aliases.

**Suggested fix.** Derive the exclusion set from the aliases rather than restating it:

```python
special_args = frozenset(('name', 'hostname', 'host', 'groupname', 'groups', 'group'))
```

This removes two variables that playbooks may be relying on today, so it is a behaviour
change — but a playbook relying on `{{ host }}` existing is relying on a leak, and the
variable it gets is a copy of the host name it already has as `inventory_hostname`.

## Related

- **Nothing keeps the two lists in sync.** `lib/ansible/modules/add_host.py` is
  documentation-only — 114 lines, no `main()`, no `AnsibleModule`, and the action plugin
  never dispatches to it. So the `aliases:` declaration is never enforced by an argspec; it
  is prose, and `special_args` is the only list with effect. The drift is structural rather
  than a typo, and the same shape can exist in any other core action plugin that documents
  aliases it filters by hand.
- **`groups:` is documented `type: list, elements: str` and also accepts a comma string.**
  The plugin splits and strips it itself (`:70-76`). Measured: `groups: [a, b]`,
  `groups: "a,b"` and `groups: "a, b"` all declare two groups, the last with the space
  stripped. Not a bug, but a second place where the declared type is not what is enforced,
  for the same reason.

## Our side

`T-177` indexes `add_host` argument keys as variable definitions. `ADD_HOST_PARAMS`
(`crates/ansible-core/src/vars.rs`) is upstream's `special_args`, **not** the documented
alias list — taking the four from the docs would have dropped `groupname` and wrongly
excluded `host`/`group`, losing two real definitions and inventing one exclusion.

So we index `host` and `group` **on purpose**, because that is what ansible does. If upstream
takes the fix above, `ADD_HOST_PARAMS` grows the two names and the behaviour becomes
version-sensitive in the usual way — the comment at the constant records the measurement, so
the change has a dated claim to compare against rather than a guess.

`demo/add_host_vars.yml` labels the leak NO HINT and reads `{{ host }}` and `{{ group }}` in
a later play; that file was run end to end on 2.21.2, and it prints
`host=ignored-as-a-name group=ignored-as-a-group` while the hosts are still named from
`name:`. The demo is therefore a live assertion of this issue, and will start failing if
upstream fixes it — which is the intended alarm.
