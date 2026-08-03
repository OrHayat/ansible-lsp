# T-070 — `inventory_dir` from real inventory sources

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P3       | M    | —          |

## Problem

`inventory_dir` was substituted with the *playbook*-dir guesses — wrong variable model.
Ansible sets it **per host**, to the directory of the inventory source that first defined
the host (`inventory/data.py:197-202`); `add_host`-created hosts get `None`
(`modules/add_host.py:62`). Sources come from `-i` (unknowable statically), the
`ansible.cfg` `inventory` key (readable — `config.rs` currently parses only
`roles_path`/`collections_path`), or the machine default. The substitution is now removed
(`expand_magic`), so `{{ inventory_dir }}` globs quietly.

## Scope

- Parse the `inventory` key in `config.rs` (comma-separated sources, same expansion rules
  as `roles_path`).
- Expand `{{ inventory_dir }}` from the dirs of concrete file/dir sources named there;
  multiple sources → multiple candidates, hit on any counts, all-miss stays quiet (per-host
  variance and `-i` overrides mean absence proves nothing).
- No key in cfg → stay templated. Never model the machine default (`/etc/ansible/hosts`).

## Done when

- [ ] `inventory` key parsed, pinned by fixture
- [ ] `{{ inventory_dir }}` resolves via cfg-named sources, quiet otherwise, never Missing
- [ ] a live run with two `-i` sources verifies the per-host first-source value, recorded
      in Settled

Source: `~/ansible_source/lib/ansible/inventory/data.py:194-202`
