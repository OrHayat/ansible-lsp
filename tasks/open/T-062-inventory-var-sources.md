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

## Done when

- [ ] `[group:vars]` and host-line `var=value` names are indexed, pinned by test
- [ ] extension-less `group_vars/<name>` / `host_vars/<name>` files are indexed
- [ ] inventory-adjacent `group_vars/`/`host_vars/` resolve via the inventory's path
- [ ] dynamic inventories are detected and skipped, never executed
- [ ] `var-undefined` stays zero-hit on the corpus with the new sources active
