# T-073 — Legacy `action_plugins/` dirs are invisible to the action-plugin check

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P3       | S    | —          |

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

- [ ] a fixture role with `action_plugins/foo.py` + a task using module `foo` hovers the
      role-local plugin, "runs on the controller"
- [ ] a cfg-declared dir is honoured, pinned by fixture
- [ ] a local plugin shadowing a collection's action plugin links the local one
