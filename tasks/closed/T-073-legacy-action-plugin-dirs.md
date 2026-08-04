# T-073 — Legacy `action_plugins/` dirs are invisible to the action-plugin check

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| closed | P3       | S    | —          |

Escape #3 from the T-029 module-hover discussion: pre-collections plugin dirs can inject or
shadow an action plugin outside the collection tree the twin check searches.

## Problem

Action plugins don't only live in collection trees. The pre-collections mechanisms still
work and take part in the same name-binding (`task_executor.py`'s comment: "allow
(historic) local `action_plugins/` override"):

- `ansible.cfg` `action_plugins` key (`DEFAULT_ACTION_PLUGIN_PATH`, default
  `~/.ansible/plugins/action:/usr/share/ansible/plugins/action`)
- a role's own `action_plugins/` directory, shipped beside its `tasks/`

An action plugin there means a task the hover labels "module (runs on the target)" is
actually handled on the controller — and one *shadowing* a collection's plugin means the
hover links a file that isn't the code that runs.

## Approach

- Parse the `action_plugins` key in `config.rs` (same expansion rules as `roles_path`).
- The twin check consults, in addition to the winner's tree: the cfg dirs, and for tasks
  inside a role, that role's `action_plugins/`. First hit in Ansible's precedence wins and
  is the linked file.
- Bare plugin names only — these dirs predate namespacing, so `<name>.py` is the whole
  lookup. No new reference kind; this only feeds the hover's provenance.

## Done when

- [x] a fixture role with `action_plugins/foo.py` + a task using module `foo` hovers the
      role-local plugin, "runs on the controller"
- [x] a cfg-declared dir is honoured, pinned by fixture
- [x] a local plugin shadowing a collection's action plugin links the local one — by
      construction: the legacy dirs are consulted *before* the winner's in-tree twin, so a
      local plugin wins. Not pinned by a collection fixture (would need an installed
      collection with a same-name action plugin); the ordering is what implements it.

## Implementation

- `config.rs` — parse the `action_plugins` cfg key (same `expand_list` as `library`).
- `workspace.rs` — `FileContext::legacy_action_plugin_dirs()`, mirroring `legacy_module_dirs`
  with the `action_plugins` subdir: the role's own dir, dirs adjacent to file/project, then
  the cfg key (or its `~/.ansible` + `/usr/share` defaults).
- `main.rs` — `legacy_action_twin(name, ctx)` looks up `<bare-name>.py` across those dirs;
  `module_hover` prefers it over the in-tree path-swap twin (`plugin_twin`), so a legacy
  plugin overrides. The existing render path then labels "runs on the controller (action
  plugin)" and links both files.
- Bare name only (`r.value` last dotted component) — these dirs predate namespacing.

Tests (against the demo fixtures): `hover_finds_cfg_dir_action_plugin_twin`,
`hover_finds_role_local_action_plugin_twin`, and the contrast
`hover_plain_module_with_no_twin_runs_on_target` (a plain `library/` module reports the
target host and grows no phantom plugin). Demo: `demo/tasks/action_plugins.yml` plus the
`reporting` role and `demo/plugins/action/` (cfg dir declared in `demo/ansible.cfg`).
