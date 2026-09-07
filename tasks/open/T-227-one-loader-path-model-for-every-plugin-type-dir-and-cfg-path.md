# T-227 — One loader-path model for every plugin-type dir and cfg path

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | S    | T-118 | —          |

## Problem

The legacy plugin-dir walk exists twice. `legacy_module_dirs` (`workspace.rs:192-215`) joins
`library/` onto the role dir, the file's dir, the project root and the `library` cfg key;
`legacy_action_plugin_dirs` (`workspace.rs:221-241`) is the same walk with `action_plugins/`.
`config.rs` reads exactly those two of the fourteen `*_plugins` path settings (`:263-264`, env
at `:298-299`). Every other plugin type Ansible loads from the same places is invisible, and
the two that are modelled were each hand-copied when their ticket came up (T-073 was the
second copy).

Where the loader looks, for every type at once (`plugins/loader.py:86-96`,
`add_all_plugin_dirs` joins each loader's `subdir` onto one path): the playbook's dir
(`playbook/__init__.py:64` — run for every playbook loaded, so an `import_playbook` target's
dir too) and each legacy role's dir as it loads (`role/__init__.py:283`; a collection-hosted
role takes the other branch at `:278` and skips this — unmeasured). After those come the
per-type cfg path — `DEFAULT_<TYPE>_PLUGIN_PATH`, ini `<type>_plugins`, env
`ANSIBLE_<TYPE>_PLUGINS`, default `~/.ansible/plugins/<type>:/usr/share/ansible/plugins/<type>`
— and the package last (`loader.py:479`, `_extra_dirs` first). Measured with vars plugins on
2.21.3 (the table is in T-228): a playbook dir and an imported playbook's dir reach every play
in the run, a `roles:` role's dir reaches every play including earlier ones, an `include_role`
role's dir reaches from the include onward, and a dir beside an included *task file* is never
read.

The full picture, surveyed 2026-09-07 against 2.21.3 (`ansible-doc -t <type> -l` counts):

| Type | Ships | We locate files | Cfg path read | Decision |
| ---- | ----- | --------------- | ------------- | -------- |
| modules | — | yes (`resolve.rs:724-810`) | `library` | modelled |
| action | — | yes (`resolve.rs:746-768`, `workspace.rs:221-241`) | `action_plugins` | modelled |
| vars | 3 | no — only `host_group_vars`' behaviour is ported (`vars.rs:1803-1854`) | no | T-228 |
| filter / test / lookup | 250 / 114 / 117 | no | no | T-115 (names), T-038, T-156 |
| strategy / connection / become | 4 / 34 / 15 | no | no | T-109 — keyword values that must name a plugin |
| inventory | 59 | sources, not plugins (`inventory.rs:301-340`) | no; `enable_plugins` unread | T-152, T-176; T-144 records the key |
| cache | 8 | no | `fact_caching` (`config.rs:286`) | done — gates `facts_persist` |
| callback / shell / terminal / cliconf / httpapi / netconf | 44 / 5 / — | no | no | runtime-only: they change how a play runs, never what it means; recorded in T-144 |
| doc_fragments | — | no | no | inside T-057 (`extends_documentation_fragment`) |
| module_utils | — | no | no | out of scope: Python imports we do not follow |

## Approach

One function, `plugin_dirs(kind, ctx)`, with a `PluginKind` carrying the strings the loader
varies — subdir name, ini key, env name, collection subdir — and the default path pair. The two
existing walks become calls to it; T-073's order (role dir, file and project dirs, cfg key,
defaults) is the order every type gets. Collection `plugins/<type>/` is the same table's
fourth string, so `resolve.rs:759-768` stops spelling `modules` and `action` by hand.

Reach is not this ticket's problem: it returns the dirs a file's context can see, in loader
order, and T-228 decides what "sees" means for a role loaded two plays later. Nothing here
changes a diagnostic; the corpus module count and the demo's action-plugin hovers are the
regression gate.

## Done when

- [ ] `legacy_module_dirs` and `legacy_action_plugin_dirs` are two calls to one per-kind
      function, and the T-073 hover tests still pass
- [ ] every `<type>_plugins` ini key and `ANSIBLE_<TYPE>_PLUGINS` env var in the table is read,
      env → ini → default per T-098, pinned by a config test per kind
- [ ] the collection `plugins/<type>/` join uses the same kind table
- [ ] the corpus scan's module line is unchanged (3665 resolved, 0 missing)
- [ ] the type table above is the register: a new plugin type lands here first, with a decision
