# T-070 — `inventory_dir` from real inventory sources

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P3       | M    | T-120 | —          |

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

**Two siblings belong here, not in tickets of their own** — same source, same per-host rule,
same `-i`-versus-cfg problem:

- **`inventory_file`** — the inventory *source* where `inventory_dir` is its directory
  (`inventory/data.py:197-202`). Whatever this ticket derives for the dir gives the file for
  free. It is also missing from `condition::MAGIC`, so T-051 has to list it regardless.
- **`ansible_inventory_sources`** — the resolved `-i` list, from `_options_vars`. Purely a
  launch fact: derivable only when the `inventory` cfg key names the sources, which is the
  same parsing this ticket already adds. With no cfg key, stay templated.

## Done when

- [ ] `inventory` key parsed, pinned by fixture
- [ ] `{{ inventory_dir }}` resolves via cfg-named sources, quiet otherwise, never Missing
- [ ] a live run with two `-i` sources verifies the per-host first-source value, recorded
      in Settled

Source: `~/ansible_source/lib/ansible/inventory/data.py:194-202`
