# T-062 — Index ini inventories and extension-less group_vars/host_vars

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P1       | M    | T-112 | —          |

## Problem

The variable index reads only YAML files with extensions, which misses two real
definition sources:

- **ini inventories** — `[group:vars]` sections and per-host `name=value` on host lines.
  Not YAML at all; a YAML-only walk sees nothing.
- **extension-less `group_vars`/`host_vars` files** — `group_vars/webservers` (no `.yml`)
  is legal and common; content is YAML but the filename filter drops it.

T-033's exploratory count was inflated by exactly this gap. Every definedness rule
sharpens with it: T-051's `var-undefined` gets fewer conceded sources, and T-060/T-061
(which this ticket blocks) need it to avoid flagging inventory-defined names.

## Approach

- Which inventory: `ansible.cfg` `inventory =` (config.rs already parses the file), else
  playbook-adjacent conventional names (`inventory`, `hosts`, `*.ini`) — deterministic
  paths only, no scanning guesses.
- ini parsing is a small hand-rolled reader: sections `[group]`, `[group:vars]`,
  `[group:children]`; `name=value` pairs; host lines with inline `var=value`. New
  `VarSource::Inventory` (host-scoped, like GroupVars).
- Inventory-adjacent `group_vars/`/`host_vars/` (next to the inventory file, not the
  playbook) become reachable once the inventory's own path is known — that closes the
  "needs the inventory's location, not guessed" gap noted in `vars.rs`.
- Extension-less group_vars/host_vars: accept files without extension in those two dirs
  specifically; parse as YAML.

## Traps

- Dynamic inventories (scripts, plugins) are executable — never run them; their vars stay
  opaque and the diagnostics' concession wording stays.
- `[group:vars]` values are strings with jinja allowed; store names only, like every
  other source.
- Multiple inventories (`inventory = a,b`) — read all listed.

## Two diagnostics that come with reading these files

Both are Ansible behaviours where the runtime produces a working-looking inventory instead of
an error, dossiered in [`upstream/ansible-inventory-silence.md`](../../upstream/ansible-inventory-silence.md).
Neither needs upstream to change; both fall out of parsing we are doing here anyway.

**A `.yml` inventory that does not parse as YAML is silently re-parsed as INI.**
`ini.verify_file` accepts any readable non-`.toml` file (`plugins/inventory/ini.py:108-110`),
and failures are reported only when *nothing* parsed (`inventory/manager.py:335`). So a YAML
syntax error yields an inventory of nonsense host names and exit 0. We already have to parse
these files; saying "this will not parse as YAML, and Ansible will not tell you" is nearly
free, and it is an ERROR — the user's inventory is not what they think it is.

**`ansible_group_priority` in a `group_vars/` file is a no-op.** It is consumed inside
`Group.set_variable` (`inventory/group.py:216-217`), reachable only from inventory-source
parsing; vars-plugin output bypasses it (`inventory/manager.py:248-249`). The variable stays
visible in `hostvars`, so it looks accepted. A WARNING on that key in a `group_vars/`/
`host_vars/` file, naming where it *would* work.

Also worth encoding while here, since it decides which file the index should read:
`group_vars/<name>/` as a **directory silently shadows** `group_vars/<name>.yml` — `''` is
probed first and the loop breaks on the first hit (`parsing/dataloader.py:470-491`). That is
the opposite of role `defaults/`, where the file shadows the directory
(`role/__init__.py:426-431`). Same function, opposite order.

## Done when

- [ ] `[group:vars]` and host-line `var=value` names are indexed, pinned by test
- [ ] extension-less `group_vars/<name>` / `host_vars/<name>` files are indexed
- [ ] `group_vars/<name>/` directories are read, and shadow the same-named `.yml`
- [ ] inventory-adjacent `group_vars/`/`host_vars/` resolve via the inventory's path
- [ ] dynamic inventories are detected and skipped, never executed
- [ ] a `.yml` inventory that fails YAML parsing is an ERROR, saying Ansible will fall back
      to INI rather than report it
- [ ] `ansible_group_priority` in `group_vars/`/`host_vars/` is a WARNING
- [ ] `var-undefined` stays zero-hit on the corpus with the new sources active
