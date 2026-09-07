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
also why an unknown name is a concession and not a silence: for the dhall plugin nothing yields
the names, and the warning has to survive for every other name in the same file.
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

**The corpus has one, and it is the collection case.** `acme.lustre.version`, enabled by FQCN
in the workspace `ansible.cfg`, `stage: inventory`, publishing `lustre_version` and
`lustre_release_commit` for every host from a submodule pin, and publishing nothing when the
checkout has neither source so `-e` can supply the pair. Measured 2026-09-07:

- our scan flags 9 uses across two playbooks as `var-undefined`, out of 133 undefined uses it
  reports for the whole corpus; the other 26 files that use
  the names are quiet only because some reachable YAML defines the same name (a role default of
  `""`, another inventory file, a `set_fact`)
- a single-file literal read finds nothing: the return is `dict(_CACHE)`, filled through a
  `derive()` in `module_utils` three calls away. Inside the plugin the names appear only as a
  shape-check table's keys and in the docstring prose. A literal reader says "open" here,
  correctly, and adds no provenance
- `ansible-inventory --list` with no extra flags shows both names on all six hosts, because the
  plugin is collection-shipped and inventory-staged; with the plugin disabled by env they are
  absent. The listing ran the derivation and printed the plugin's own divergence warning, which
  is the execution-risk reminder: user gesture only

**[Ansible vars plugins — OneUptime](https://oneuptime.com/blog/post/2026-01-30-ansible-vars-plugins/view)**
is the tutorial shape: `vars_plugins = ./plugins/vars`, `vars_plugins_enabled = host_group_vars,
postgres_vars, vault_vars`, `run_vars_plugins = start|demand`, a per-plugin `[postgres_vars]` ini
section, and `REQUIRES_ENABLED`. Its four examples pull names from a database table, an API
response, Vault secret paths, and env vars plus JSON files — none of the four has its names in
the Python, so rung 2 finds nothing for any of them, which is more evidence for the ladder's
shape. It also states a precedence rule, "vars plugins load after group_vars files but before
host_vars files", which is wrong on both halves — measured below.

## Precedence, measured

2.21.3, default `VARIABLE_PRECEDENCE`, one plugin that answers differently per entity so a
same-rung tie is distinguishable from a later-rung win. `scratchpad/t228_vars_plugin_precedence.sh`
reruns it, with a not-enabled control; `scratchpad/t228_review_stage.sh` (from the independent
review) adds the stage dimension. The merge is `vars/manager.py:288-299`: the precedence
rungs `all_inventory, groups_inventory, all_plugins_inventory, all_plugins_play,
groups_plugins_inventory, groups_plugins_play`, then `host.get_vars()`, then the plugins for the
host, inventory-adjacent then play-adjacent.

**Stage comes first.** A plugin that runs at the inventory stage — `stage: inventory`, or no
`stage` under `run_vars_plugins = start` — has its output baked into `group.vars` / `host.vars`
while the inventory parses (`inventory/manager.py:249-251`), so it lands on the `groups_inventory`
and `host.get_vars()` rungs, *below* the task-stage plugin that loads `group_vars/` and
`host_vars/`. A task-stage plugin (the default) lands on the plugin rungs beside that loader,
where list order decides.

| Collision                                                    | Task-stage plugin (default)                                                | Inventory-stage plugin                                                |
| ------------------------------------------------------------ | -------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| plugin's answer for the host vs its answer for a group       | host — a later rung, whatever the order                                    | host — same                                                           |
| plugin's answer for `all` vs its answer for a child group    | the child group                                                            | the child group; and it loses to an inline `[web:vars]` var too       |
| plugin vs `group_vars/<g>.yml`, same entity                  | whichever is **later in `vars_plugins_enabled`**                           | the file, whatever the order                                          |
| plugin's host answer vs `group_vars/<g>.yml`                 | plugin — a later rung                                                      | plugin — the inventory host rung is still above every group rung      |
| plugin vs `host_vars/<h>.yml`, same entity                   | whichever is later in the list                                             | the file, whatever the order                                          |
| plugin's host answer vs an inline inventory host var         | plugin — `host.get_vars()` merges first (`manager.py:297-298`)             | plugin — combined after the parse (`inventory/manager.py:251`)        |
| plugin vs role `defaults/` / role `vars/` / play `vars:`     | plugin / role vars / play vars                                             | same                                                                  |

The list-order tie is measured with the plugin listed. An *unlisted* legacy plugin is primed
first (`vars/plugins.py:18-23`) and loses every same-rung tie — `all`, a child group and the host
— measured by the precedence script's `not listed` run.

**On the corpus plugin, measured.** Collection-shipped, `stage: inventory`, listed after
`host_group_vars`, answering for every entity. A temporary inventory outside the repo, two
controls: `lustre_version` in `group_vars/all.yml` **loses** to the plugin, because the plugin's
host answer sits above every group rung; `lustre_release_commit` in `host_vars/<host>.yml`
**beats** the plugin, because the file loads at task stage and the plugin's host answer was baked
in at inventory stage. The earlier "listed after, so it wins" reading was wrong on the host_vars
half; the review's "inventory stage loses to the files" was wrong on the group_vars half. Only
the run settled it.

Consequence for hover: the winner depends on the plugin's stage (readable — DOCUMENTATION
default, its ini section, env, or `run_vars_plugins`), the entity it answered for (not readable),
and its list position (readable). Hover can state the rule and name the plugin; it cannot rank
the value unless rung 3 ran the plugin. The first probe of this section got the `all` row wrong
for an hour because the plugin also answered for `ungrouped`, which lands a rung later — rule 2.

## Beyond the fix — every option, cheapest first

The fix above is rung 1. The rest is how the tool learns *which* names a plugin publishes, so
those names become definitions instead of hedged warnings. The rungs are independent; the
per-name rule is the same under all of them.

1. **Know it is there.** Locate, enabled list, stage, reach, then the per-name rule. With no
   names known, every use in reach keeps a hedged warning.

2. **Read the Python.** A bounded chase over the plugin's syntax tree — ruff's parser crates in
   process, or the install's `ast` module through a cached subprocess; neither executes
   anything. Seven rules, each pinned by a test:
   - the returned expression is a dict literal (its keys are names), or `X` / `dict(X)` for a
     variable `X`, which is then tracked
   - `X[literal] = …` adds a name
   - `X.update(EXPR)` adds what `EXPR` yields; `EXPR or {}` unwraps to `EXPR`
   - a call to a function in the same file, or imported from the same collection's
     `module_utils`, is followed into its return statements, to a depth limit
   - a returned dict literal yields its keys
   - a tracked dict passed as an argument is tracked as that parameter inside the callee
   - conditions are not evaluated: every branch contributes, so a name the plugin *may* publish
     counts as known — the silence direction, the same as `-e`

   Checked against real code: the corpus plugin yields 3 of 3 (`dict(_CACHE)`, one `update`
   through `derive()` to `_fields` one file away, one literal subscript); lilatomic's `gitroot`
   and `knownhostentry` yield their one name each with no chase; `dhall_vars` yields nothing.
   What the chase cannot promise: a shape outside the rules — `update(**kw)`, a key built with
   `%`, a loop over a list of names — yields "unknown", never a wrong name. Feeds hover
   provenance, go-to-definition, and completion once T-127 exists.

3. **Read the data files a file-backed plugin reads.** A plugin that is group_vars in another
   format — lilatomic's `dhall_vars`, any JSON/TOML/INI loader — calls
   `loader.find_vars_files(path=…, name=entity.name, extensions=[…])` under a literal
   `"host_vars"` / `"group_vars"`, so rung 2's reader recognises the shape and the extension
   list (an editor setting can say the same for a plugin whose code is less tidy). The
   group_vars walk then reads those files with a reader per format: JSON, TOML and INI give
   exact names; Dhall gives them for a plain record `{ port = 80 }` and says "unknown" for a
   file built from a `let`, an import or a `//` merge. Definitions land on the data file, with
   the plugin named in hover — better than the plugin file, because the value is there.
   Precedence is the plugin's, since the plugin returns the values.

4. **Ask Ansible on a gesture.** Two mechanisms, both measured:
   - `ansible-inventory --list [--playbook-dir <dir>]` runs cfg-path, collection and, given the
     dir, playbook-adjacent plugins, and prints every host's variables. The corpus plugin's
     three names on all six hosts came out this way, with the plugin disabled as the control.
     Role-local plugins do not show, because no playbook is loaded.
   - `scratchpad/t228_varsdump.py`: 25 lines over Ansible's API that load the playbook as
     `ansible-playbook` does — which is what adds the role plugin dirs — then ask the variable
     manager for each host's variables per play. No task runs. On the fixture it lists the role
     plugin's name in play 1, matching the reach table; renaming the role's dir removes it.

   Only the plugins execute, with the editor's environment, so this is a button, cached, age
   shown, never automatic — T-176's command extended to vars plugins. It gives exact names
   *and* values, so hover can rank precedence for real. It is the only route for a data-driven
   plugin — database rows, an API, Vault — whose names do not exist until runtime, and one
   press clears every hedged warning such a plugin caused.

5. **Declare.** For a plugin one owns, `PROVIDES` (Fix, above): a literal tuple at class level,
   self-checked, read by rung 2 with no chase. A convention of ours, not Ansible's, and
   optional — rung 2 reads the corpus plugin without it. For a plugin one does not own, the
   same list in an editor setting mapping the plugin to its names. A declared set is closed:
   declared names are definitions, everything else keeps its plain warning.

What each rung gives for the real cases:

| Plugin                                  | Rung 2          | Rung 3                            | Rung 4                                | Rung 5                          |
| --------------------------------------- | --------------- | --------------------------------- | ------------------------------------- | ------------------------------- |
| corpus `acme.lustre.version`            | 3 of 3 by chase | not file-backed                   | all three, no playbook dir needed     | one line                        |
| lilatomic `gitroot`, `knownhostentry`   | 1 of 1 each     | —                                 | yes                                   | an upstream PR                  |
| lilatomic `dhall_vars`                  | nothing         | plain records yes, computed no    | yes                                   | only if the user knows the names |
| a database / API / Vault plugin         | nothing         | —                                 | yes — the only route                  | nobody knows the names          |

Not pyo3: the metadata holds no names (no `RETURN` for vars plugins), so "inspect" means
import, which executes module-level code, and linking a libpython into the server binary buys
nothing a subprocess does not.

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
| a role's `vars_plugins/`, role in `roles:` of any play — or pulled in by `import_role` or a `meta/main.yml` dependency | every play, including those before the role's play (`role/__init__.py:283` runs at load) |
| the same role reached only by `include_role` | the include onward in **execution** order (`pre_tasks`, `roles`, `tasks`, `post_tasks`, handlers), not text order; an earlier play sees nothing, and under a false `when:` no later play does either |
| a collection-hosted role's `vars_plugins/` | nothing, ever — `role/__init__.py:278` takes the other branch and never calls `add_all_plugin_dirs` |
| a dir on the `vars_plugins` cfg key | every play |
| `vars_plugins/` beside an included task file | nothing — never read |
| a collection's `plugins/vars/` | nothing until the FQCN is in `vars_plugins_enabled`; then every play. A `REQUIRES_ENABLED` attribute there, either value, only draws a warning (`vars/plugins.py:60-66`; read, not run) |
| a legacy plugin with `REQUIRES_ENABLED = True` | nothing until its name is in `vars_plugins_enabled` |
| the dir removed (control) | fatal, undefined |

**Every row above is at the default task stage.** With `stage: inventory` on the plugin, or
`run_vars_plugins = start` for a plugin that sets no `stage`, the playbook-adjacent, imported-
playbook and role rows all become *nothing*: those dirs join the loader after the inventory has
parsed, and an inventory-stage plugin never runs again (`scratchpad/t228_review_stage.sh`, from
the independent review). Only the cfg-path and collection rows survive an inventory-stage plugin.
The review also reports that an inventory given as a host list (`-i localhost,`) gives an
inventory-stage plugin no path to run for (`vars/plugins.py:82`); not re-verified here.

The names a plugin yields are not knowable statically — they come out of Python (the article's
example computes them from git). So the legal set is *open* wherever an active plugin reaches,
the same shape as a declined dynamic inventory, which `declined_inventories` (`vars.rs:1751`)
already records so that "no hosts" and "hosts unknown" stay apart.

## Fix

Locate, record, never run. T-227 supplies the dirs; this ticket adds the enabled list and the
`REQUIRES_ENABLED` / collection rules above, and records the active custom plugins per file the
way declined inventories are recorded — reach per the table, so a role-local plugin reaches
every playbook that lists the role and an `include_role` one reaches only what follows.

What the record changes is how `var-undefined` treats a name in reach, and the corpus decides
it: the scan reports 133 undefined uses there, 9 of them the plugin's. Silencing every file the
plugin reaches — the whole project, since it is enabled from the cfg — would throw away 124
findings to fix 9. So the rule is per name, never per file:

- **Known names are a definition source.** When the plugin's names are known — from a
  `PROVIDES` declaration in the plugin (below), a user setting for plugins one does not own, the
  literal read, or a press-to-run result — each is a `VarSource::VarsPlugin`
  definition located at the plugin file: `var-undefined` is satisfied, hover and go-to-definition
  land on the plugin, and every other name keeps its warning.
- **Unknown names are a concession, not a silence.** When nothing yields the names, the warning
  stays and the message names the plugin as a possible source, beside the inventory, facts and
  `-e` concessions it already makes. That is a hedge on working code for the plugin's own names,
  and it is the price of keeping the other 124.
- **Never a per-file or per-project silence.** The declined-inventory silence (T-062 box 8) is
  for a *host list* we cannot see, where every `hostvars[...]` would be wrong; a plugin's names
  are a handful inside a set of thousands, and the feature is worth more than the handful.

`host_group_vars` is not "custom" — it is the ported behaviour and must not trip the record.

**An optional convention for plugins one owns: `PROVIDES`.** Ours, not Ansible's, and not
required — rung 2 below reads the corpus plugin's names without it; this is insurance against
the dict-filling growing past the chase's rules. A class-level tuple of string literals on
`VarsModule` naming every variable the plugin can publish, with the plugin checking itself
against it so the list cannot go stale:

```python
class VarsModule(BaseVarsPlugin):
    PROVIDES = ("lustre_version", "lustre_version_short", "lustre_release_commit")

    def get_vars(self, loader, path, entities):
        ...
        published, declared = set(out), set(self.PROVIDES)
        if not published.issubset(declared):
            raise AnsibleError("undeclared names: %s" % sorted(published.difference(declared)))
        return out
```

Measured 2.21.3: a plugin carrying the attribute and the check runs unchanged; the name is
unused anywhere in core and `BaseVarsPlugin` defines only `is_stateless`. The reader wants
exactly that shape — a literal tuple at class level, no call, no comprehension — and treats a
declared set as *closed*: the declared names are definitions at the plugin file, every other
name keeps its plain warning with no hedge. The corpus plugin is the motivating case: its names
are three calls away in `module_utils` and unreadable, and one line makes them readable. A
plugin one cannot edit gets the same effect from the user setting.

Traps:

- `stage` / `run_vars_plugins` decide *whether* a plugin exists, per location — the stage note
  under the reach table. The reader takes `stage` from the plugin's DOCUMENTATION default, its
  ini section and env, and `run_vars_plugins` from the cfg, before applying the table.
- `REQUIRES_ENABLED` on a legacy plugin is Python. A literal class-level `REQUIRES_ENABLED = True`
  is greppable; anything else marks the plugin *uncertain*. With names known, an over-recorded
  plugin is a false silence, so an uncertain plugin's names take the concession path, never the
  definition path.
- The demo: a `vars_plugins/` directly under `demo/` puts every demo file in reach and silences
  the `var-undefined` assertions there. The fixture needs its own playbook dir (`demo/vars-plugin/`
  or similar) so reach stays inside it; rule 4's `every_other_demo_file_is_free_of_<rule>` guard
  is what proves that.

## Done when

- [ ] the five locations and the enabled list are read, with the ini/env spellings above, and
      `host_group_vars` never counts as custom
- [ ] reach follows the table — `roles:`, `import_role`, `meta/main.yml` dependencies and
      `import_playbook` playbook-wide, `include_role` forward-only in execution order and nothing
      under a false `when:`, an included task file's dir and a collection-hosted role's dir
      nothing — one test per row
- [ ] `stage` and `run_vars_plugins` are read, and an inventory-stage plugin reaches only from
      the cfg-path and collection rows, pinned by the stage script's rows
- [ ] a demo fixture in its own playbook dir has a plugin-provided use; the test asserts the
      exact diagnostics for it, and every other demo file's diagnostics are unchanged
- [ ] hover on a name in reach names the plugin file, and go-to-definition on a *known* name
      lands on the plugin file (the file, not a byte span into it); an unknown name stays
      unnavigable
- [ ] every reader of the index answers for `VarSource::VarsPlugin` on purpose (rule 3): the
      exhaustive matches (`vars.rs:81`, `:131`, `:193`, `main.rs:2900`, `:2926`), the
      non-exhaustive ones that would silently omit it (`vars.rs:158` `host_scoped`, `:883`
      `value_span_source`), and the readers that open the definition file as YAML or take a byte
      span into it (`main.rs:1687`, `:1764`, `:2453`, `:2516`, `:2684`, `:2768`, `:4335`) never
      open a `.py` as a document — one test per reader
- [ ] the scan output lists active vars plugins per project root, next to its config line
- [ ] no plugin is ever executed, pinned the way T-176 pins it
- [ ] a known name is a definition at the plugin file, an unknown name keeps its warning with
      the plugin conceded in the message, and no file is ever silenced wholesale — pinned by a
      test with two names in one file, one declared and one not
- [ ] the corpus scan's undefined count moves by exactly the plugin's names once they are
      declared or read, and by zero before that
