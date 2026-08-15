# T-062 — Index ini inventories and extension-less group_vars/host_vars

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| partly done | P1  | M    | T-112 | —          |

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

## Measured (2.21.2)

One ini inventory, one play, every source present at once — all four confirmed rather than
read off the source:

| written                                            | reaches the play as        |
| -------------------------------------------------- | -------------------------- |
| `node1 host_line_var=...` (inline on the host line) | `FROM_HOST_LINE`           |
| `[webservers:vars]` section                         | `FROM_GROUP_VARS_SECTION`  |
| `group_vars/all` (no extension)                     | `FROM_EXTENSIONLESS`       |
| `group_vars/webservers.yml` **and** `.../webservers/main.yml` | `FROM_DIRECTORY` |

The last row is the one to keep in mind while writing the reader: with both spellings
present the **directory wins** and the `.yml` file is silently dead. That is the opposite of
role `defaults/`, where the file shadows the directory — same `DataLoader` function, opposite
probe order (`parsing/dataloader.py:470-491` vs `role/__init__.py:426-431`). Indexing the
`.yml` when a directory exists beside it would report a value no run ever uses.

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

**A `hostvars['name']` for a host the inventory does not have is fatal, and Ansible does not
say why.** Measured on 2.21.2 against a two-host ini inventory:

```
'web0143' in hostvars  ->  False          'node1' in hostvars  ->  True
{{ hostvars['web0143'].infiniband_ip }}   ->  fatal: hostvars['web0143']
```

The error is the literal subscript — `_undef(f"hostvars[{host_name!r}]")`,
`vars/hostvars.py:55` — with no hint that the host is the problem rather than the variable.
Once the inventory is parsed the host list is enumerable, which makes this a cleaner claim
than T-172's: a name either is a host or is not.

Escapes that must keep it quiet, none of them optional: a dynamic inventory , `add_host` anywhere in the play, and more `-i`
files than we resolved. Also `hostvars` auto-creates the implicit localhost on membership
(`:69-71`) while `list(hostvars)` omits it, so `localhost` must never be flagged.

Also worth encoding while here, since it decides which file the index should read:
`group_vars/<name>/` as a **directory silently shadows** `group_vars/<name>.yml` — `''` is
probed first and the loop breaks on the first hit (`parsing/dataloader.py:470-491`). That is
the opposite of role `defaults/`, where the file shadows the directory
(`role/__init__.py:426-431`). Same function, opposite order.

## Done when

- [x] `[group:vars]` and host-line `var=value` names are indexed, pinned by test
- [x] extension-less `group_vars/<name>` / `host_vars/<name>` files are indexed
- [x] `group_vars/<name>/` directories are read, and shadow the same-named `.yml`
- [x] inventory-adjacent `group_vars/`/`host_vars/` resolve via the inventory's path
- [x] dynamic inventories are detected and handled — detected, never executed, and the skip
      **recorded** rather than silent (`vars::declined_inventories`), so "read it, no such
      host" and "did not read it, hosts unknown" stay apart; box 8 below depends on that
      distinction. Shown in the inventory panel and the status bar, so the blind spot is
      visible rather than merely known. Running one on an explicit user command is **not**
      here — that is T-176.
- [x] a `.yml` inventory that fails YAML parsing is an ERROR, saying Ansible will fall back
      to INI rather than report it. The INI half of the same rule is already written and
      `#[ignore]`d: `an_unknown_section_type_discards_the_whole_ini_file` — a `[web:var]`
      typo makes Ansible discard the **whole file**, measured, while we keep reading it and
      invent a name. Unignore it with this box. Done: `inventory-not-yaml` replaces the
      generic `unparseable` ERROR on a resolved inventory source, and `ini_vars` defines
      nothing from a file with an unknown section tag (`[web:hosts]` measured valid and
      kept as the control). Scope note, measured: the *silent* fallback needs the lines to
      be INI-acceptable — a bare `key:` line fails the ini plugin too (warnings, still exit
      0); dossier updated to match.
- [ ] `ansible_group_priority` in `group_vars/`/`host_vars/` is a WARNING
- [ ] `hostvars['name']` for a host no parsed inventory has is an ERROR naming the host —
      silent under a dynamic inventory, any `add_host`, or an unresolved `-i`, and never
      for `localhost`
- [x] `var-undefined` stays zero-hit on the corpus with the new sources active
