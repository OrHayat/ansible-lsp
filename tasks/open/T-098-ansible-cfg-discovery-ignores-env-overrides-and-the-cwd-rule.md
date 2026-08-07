# T-098 — ansible.cfg discovery ignores env overrides and the CWD rule

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | S    | T-090 | —          |

## Symptom

We read `roles_path` and `collections_path` from an `ansible.cfg` the user's actual
invocation may be ignoring, and we ignore the environment variables that override it
outright. Every role and collection path downstream inherits the error.

## Cause

`find_ini_config_file` (`config/manager.py:253-313`) is `ANSIBLE_CONFIG` → `./ansible.cfg` in
the **CWD only** → `~/.ansible.cfg` → `/etc/ansible/ansible.cfg`. First hit wins, no merging,
and **no ancestor walk**. `workspace.rs:217-221` walks up from the file.

Also unmodelled: `ANSIBLE_ROLES_PATH`, `ANSIBLE_COLLECTIONS_PATH` and `ANSIBLE_LIBRARY`
override the ini wholesale, while `config.rs` reads only `ANSIBLE_NETWORK_GROUP_MODULES`. And
a world-writable CWD makes its `ansible.cfg` silently skipped (`manager.py:279-284`).

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

- [ ] `ANSIBLE_CONFIG` is honoured when set
- [ ] the three path env vars override the ini
- [ ] the ancestor walk is commented as an editor heuristic, not Ansible behaviour
- [ ] the scan report says which config file was used
- [ ] `{{ ansible_config_file }}` expands to the discovered file, and stays templated when
      none was found
