# T-098 — ansible.cfg discovery ignores env overrides and the CWD rule

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-090 | —          |

## Symptom

We read `roles_path` and `collections_path` from an `ansible.cfg` the user's actual
invocation may be ignoring, and we ignore the environment variables that override it
outright. Every role and collection path downstream inherits the error.

## Cause

`find_ini_config_file` (`config/manager.py:253-313`) is `ANSIBLE_CONFIG` → `./ansible.cfg` in
the **CWD only** → `~/.ansible.cfg` → `/etc/ansible/ansible.cfg`. First hit wins, no merging,
and **no ancestor walk**. `workspace.rs:217-221` walks up from the file.

The complete env surface, audited against 2.21.2: `manager.py` reads the environment in
exactly two places — `ANSIBLE_CONFIG` directly (`manager.py:266`) and a loop over each
setting's `env:` list from `base.yml` (`manager.py:647`) — so the vars below are provably
all of them for the settings `config.rs` consumes:

| Env var                           | Overrides                          | Modelled today?     |
| --------------------------------- | ---------------------------------- | ------------------- |
| `ANSIBLE_CONFIG`                  | which config file is read at all   | yes, `config.rs:201` |
| `ANSIBLE_HOME` (ini: `home`)      | the `~/.ansible` half of every path default | yes, `config.rs:164` (`install.rs:304` env-only: no project cfg in scope there) |
| `ANSIBLE_ROLES_PATH`              | `roles_path`                       | yes, `config.rs:152` |
| `ANSIBLE_COLLECTIONS_PATH`        | `collections_path`                 | yes, `config.rs:155` |
| `ANSIBLE_LIBRARY`                 | `library`                          | yes, `config.rs:158` |
| `ANSIBLE_ACTION_PLUGINS`          | `action_plugins`                   | yes, `config.rs:161` |
| `ANSIBLE_NETWORK_GROUP_MODULES`   | `network_group_modules`            | yes, `config.rs:145` |
| `ANSIBLE_DUPLICATE_YAML_DICT_KEY` | `duplicate_dict_key`               | yes, `config.rs:149` |

Each override replaces the ini value wholesale — no merging. And a world-writable CWD makes
its `ansible.cfg` silently skipped (`manager.py:279-284`).

## Approach

Honour the env overrides first, then the ancestor walk. Keep the walk — a repo's config is
what the user means even when they run from a subdirectory — but say in the code that it is
our heuristic and not Ansible's, so the next reader doesn't take it for modelled behaviour.

## Free once this lands: `{{ ansible_config_file }}`

`_get_magic_variables` sets `ansible_config_file = C.CONFIG_FILE` (`vars/manager.py:457`) —
the config file actually in effect. It is one of the three magic variables that need no play,
host or task, so like `playbook_dir` it is available even at playbook parse time.

Today we do not substitute it, and could not honestly: we read a config file the invocation
may be ignoring, which is this ticket's whole complaint. Once discovery matches
`find_ini_config_file`, the value is simply *the file we found*, so expanding it costs
nothing beyond adding it to `expand_magic`. Rare in a path, but exact when it appears —
unlike the guessed variables, there is nothing to approximate.

Corollary: if discovery finds **no** config file, `C.CONFIG_FILE` is `None` and the variable
is undefined at runtime. Do not substitute an empty string; leave it templated.

## Done when

- [x] `ANSIBLE_CONFIG` is honoured when set — `env_config_file` (`config.rs:201`); unit-tested via the
      `EnvMap` seam, real-env plumbing in `tests/process_env_snapshot.rs`
- [x] `ANSIBLE_ROLES_PATH` overrides the ini's `roles_path`
- [x] `ANSIBLE_COLLECTIONS_PATH` overrides the ini's `collections_path`
- [x] `ANSIBLE_LIBRARY` overrides the ini's `library`
- [x] `ANSIBLE_ACTION_PLUGINS` overrides the ini's `action_plugins`
- [x] `ANSIBLE_HOME` (env, or the `home` ini key) relocates the hardcoded `~/.ansible`
      defaults in `workspace.rs` and `install.rs` — `AnsibleConfig::ansible_home`, resolved
      env → ini → `~/.ansible`; install discovery honours the env half only, having no
      project cfg in scope
- [ ] the ancestor walk is commented as an editor heuristic, not Ansible behaviour
- [ ] the scan report says which config file was used
- [ ] `{{ ansible_config_file }}` expands to the discovered file, and stays templated when
      none was found
