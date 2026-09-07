# T-228 — A variable from an active vars plugin is reported undefined

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-112 | T-227      |

## Symptom

A `vars_plugins/` beside the playbook holds one plugin whose `get_vars` returns
`{"from_pb_plugin": ...}`, and a task reads the name. Ansible prints the value. Our scanner:

```
UNDEFINED VARIABLES (1):
  play.yml:4  from_pb_plugin
```

The control with the dir removed fails in Ansible with `'from_pb_plugin' is undefined`, so the
plugin is the only source. `scratchpad/t228_vars_plugin_probe.sh` rebuilds the fixture and
runs every row of the table below.

This is the case [lilatomic's "Writing a vars plugin"](https://www.lilatomic.ca/posts/ansible_writing_facts_plugin/)
describes — ambient variables with no YAML definition anywhere. Every name such a plugin
provides is a false WARNING on working code, and `# noqa` only makes the tool quiet, not right.

## In the wild — two write-ups, one open set

Both articles below are answers to the same complaint: YAML-only variable sources are not
enough for a real estate. Both make the variable set unknowable from YAML, by different routes,
and a tool that indexes YAML alone calls their variables undefined.

**[Writing a vars plugin — lilatomic](https://www.lilatomic.ca/posts/ansible_writing_facts_plugin/)**
is the plugin proper. The motive: "often there are facts you wish were just ambiently
available, especially if you are working with cloud infrastructure" — subscription and tenant
ids, the contents of a resource group. A `set_fact` task could supply them, but that
"severely limits your ability to resume at a task"; a vars plugin has the values in place
before any task runs. What the article shows, and what each piece means for us:

| The article                                                                                                              | For this ticket                                                                                                                          |
| ------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `get_vars` runs at the inventory stage once per host and group, and at the task stage once per entity × inventory path — the first draft "is really bad, because it gets executed *a lot*", so every plugin caches in a module-level dict | how often it runs is the plugin's problem, not ours; the names exist by task time either way — the `stage` trap in Fix                    |
| the `stage` option comes from the `vars_plugin_staging` doc fragment, settable per plugin through an ini section or an env var | a plugin-specific ini section is not one of `config.rs`'s two sections; T-227's reader needs to at least not choke on it                |
| the plugins ship in a collection (`lilatomic.alpacloud.*`)                                                              | nothing runs until the FQCN is in `vars_plugins_enabled` — row 7 of the table, and the enabled list is a hard requirement, not a refinement |
| `gitroot` returns `{"src": <git toplevel>}`                                                                              | one literal name, greppable in the Python                                                                                                 |
| `knownhostentry` loops the entities and returns `{"known_hosts": ...}` per host                                          | one literal name, a per-host value                                                                                                        |
| `dhall_vars` reads `host_vars/` and `group_vars/` `.dhall` files and merges them with `combine_vars`                     | names that live in a format we do not parse — no static read of the plugin finds them                                                    |

Three plugins, three different name sources, and only two of the three are readable off the
Python. That is the case against a best-effort literal-dict index standing in for the truth: it
would find `src` and `known_hosts` and miss every dhall name, and then warn on those. It is
also why option A is the recommendation — silence in reach is right for all three at once.
Nothing in the article says how a reader later finds where `src` came from; hover naming the
plugin file is the whole of what a tool can add there.

**[Thinking outside the box with Ansible — G-Research](https://www.gresearch.com/news/thinking-outside-the-box-with-ansible/)**
is not a vars plugin, and reaches the same place. The complaint: "group vars are all loaded
together indeterminately so it doesn't really give you a good way to have a hierarchical
structure where things at lower levels can override settings at higher levels", which "tends to
lead to hacky solutions that try to implement variable overriding, but lead to messy code". The
fix is a hierarchy of their own — region → environment → product → service → customer — laid
out as `vars/region/<r>/vars.yml`, `vars/env/<e>/vars.yml`, `vars/product/<p>/vars.yml`, with
each host carrying its scope as inventory group tags or cloud tags. A `loadvars` role included
at the top of every playbook, backed by a helper library that "determines which vars files
should be loaded and in what order", walks that hierarchy with `include_vars`; lower levels
override higher ones, and `hash_behaviour: merge` makes dicts merge key by key instead of
replacing. The author's own caveat: "the hierarchy isn't always fixed in stone".

For us every one of those variables arrives through an `include_vars` whose path is computed
at run time from host metadata. That is T-017's templated-path case, and the honest answer
there is this ticket's: from the `loadvars` role onward the set is open, and a warning on a
name it may have loaded is the same false squiggle. T-017 should say so. `hash_behaviour:
merge` is the other half — T-144 already routes it to T-112, and it changes what a dotted
access resolves to (T-221).

## Cause

Nothing reads a vars plugin location. The `host_group_vars` plugin's *behaviour* is ported
(`vars.rs:1803-1854`, `inventory.rs:159-180`), and `vars_plugins/` appears only as a name to
skip when expanding an inventory dir (`inventory.rs:50`). None of `VARIABLE_PLUGINS_ENABLED`
(ini `vars_plugins_enabled`, env `ANSIBLE_VARS_ENABLED`, default `host_group_vars`),
`DEFAULT_VARS_PLUGIN_PATH` (ini `vars_plugins`, env `ANSIBLE_VARS_PLUGINS`) or `RUN_VARS_PLUGINS`
is in `config.rs`, and no walk joins `vars_plugins/` onto anything (T-227).

Measured on 2.21.3, one plugin per location, each row with a control:

| Plugin at | Reaches |
| --------- | ------- |
| `vars_plugins/` beside the playbook | every play; nothing to enable (`playbook/__init__.py:64`) |
| `vars_plugins/` beside an `import_playbook` target | every play, including those before the import |
| a role's `vars_plugins/`, role in `roles:` of any play | every play, including those before the role's play (`role/__init__.py:283` runs at load) |
| the same role reached only by `include_role` | the include onward; an earlier play sees nothing |
| a dir on the `vars_plugins` cfg key | every play |
| `vars_plugins/` beside an included task file | nothing — never read |
| a collection's `plugins/vars/` | nothing until the FQCN is in `vars_plugins_enabled`; then every play. `REQUIRES_ENABLED` is ignored there, with a warning (`vars/plugins.py:62-67`) |
| a legacy plugin with `REQUIRES_ENABLED = True` | nothing until its name is in `vars_plugins_enabled` |
| the dir removed (control) | fatal, undefined |

The names a plugin yields are not knowable statically — they come out of Python (the article's
example computes them from git). So the legal set is *open* wherever an active plugin reaches,
the same shape as a declined dynamic inventory, which `declined_inventories` (`vars.rs:1751`)
already records so that "no hosts" and "hosts unknown" stay apart.

## Fix

Locate, record, never run. T-227 supplies the dirs; this ticket adds the enabled list and the
`REQUIRES_ENABLED` / collection rules above, and records the active custom plugins per file the
way declined inventories are recorded — reach per the table, so a role-local plugin reaches
every playbook that lists the role and an `include_role` one reaches only what follows.

What the record changes is the open decision:

- **A — silence in reach.** `var-undefined` does not fire in a file an active custom plugin
  reaches, and hover on an undefined name says which plugin file may define it. Never wrong;
  loses definedness coverage in exactly the files where we cannot have it.
- **B — warn and concede.** The warning stays and the message adds "or vars plugin `x.py`".
  Keeps the coverage; every name the plugin does provide is still a false squiggle on working
  code, which is the thing this repo says costs more than silence.

Recommendation is A, on the repo's own rule. Either way `host_group_vars` is not "custom" — it
is the ported behaviour and must not trip the record.

Traps:

- `stage` / `run_vars_plugins` decide *when* a plugin runs (inventory or task), not whether its
  names exist by task time. Assumed irrelevant to definedness; measure before relying on it.
- `REQUIRES_ENABLED` on a legacy plugin is Python. A literal class-level `REQUIRES_ENABLED = True`
  is greppable; anything else counts as enabled — the safe direction, since an over-recorded
  plugin only silences.
- The demo: a `vars_plugins/` directly under `demo/` puts every demo file in reach and silences
  the `var-undefined` assertions there. The fixture needs its own playbook dir (`demo/vars-plugin/`
  or similar) so reach stays inside it; rule 4's `every_other_demo_file_is_free_of_<rule>` guard
  is what proves that.

## Done when

- [ ] the five locations and the enabled list are read, with the ini/env spellings above, and
      `host_group_vars` never counts as custom
- [ ] reach follows the table — `roles:` and `import_playbook` playbook-wide, `include_role`
      forward-only, an included task file's dir nothing — one test per row
- [ ] a demo fixture in its own playbook dir has a plugin-provided use; the test asserts the
      exact diagnostics for it, and every other demo file's diagnostics are unchanged
- [ ] hover on a name in reach names the plugin file; nothing is navigable, because there is
      nothing to navigate to
- [ ] the scan output lists active vars plugins per project root, next to its config line
- [ ] no plugin is ever executed, pinned the way T-176 pins it
- [ ] A or B is chosen and the message wording is in this ticket before the rule lands
