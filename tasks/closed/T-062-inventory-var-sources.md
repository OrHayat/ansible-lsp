# T-062 — Index ini inventories and extension-less group_vars/host_vars

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| done   | P1       | M    | T-112 | —          |

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
- [x] `ansible_group_priority` in `group_vars/`/`host_vars/` is a WARNING —
      `group-priority-ignored`, on the key span, suppressible. Measured on 2.21.2 with the
      control the dossier lacked: the same key in an inventory source **does** move the merge
      winner, so the rule is scoped to the vars-plugin directories and nothing else. The tell
      is inverted — where it works it is consumed and never becomes a variable; where it is
      inert it survives as an ordinary one, which is the only thing the user can see. Dossier
      updated with the table, and with the templated case (warns and falls back at 2.21.2, it
      does not raise as written). Zero hits on the corpus. Scoped deliberately: the key is
      equally inert in `vars_files:`/play `vars:`, which this does **not** flag — that is a
      wider claim than the box, and it is where people write it that matters.
- [x] `hostvars['name']` for a host no parsed inventory has is an ERROR naming the host —
      silent under a dynamic inventory, any `add_host`, or an unresolved `-i`, and never
      for `localhost`. Done: `unknown-host`, on the host-name span, suppressible. The host
      list comes from new `ini_hosts`/`yaml_hosts`/`toml_hosts` beside the `*_vars` readers,
      through `vars::inventory_hosts`, which returns **`Option`** — unknowable is a third
      answer, not a "no", and every escape is that `None`.

      Measured first, all on 2.21.2. Ansible's message is the reason this earns an ERROR: a
      bad host gives `Error while resolving value for 'msg': hostvars['web0143']` — the
      expression echoed back — while the control, a real host with a missing variable, says
      `object of type 'HostVarsVars' has no attribute 'infiniband_ip'`. Only the typo case
      is cryptic. Host patterns expand the way ansible expands them (`web[01:05]` inclusive
      and padded, `rack[a:c]`, `s[00:10:5]`, cartesian across two brackets) — and **YAML
      inventories expand them too**, which the ini plugin owning the syntax would not have
      suggested. `localhost` is never flagged: `'localhost' in hostvars` is True with no
      inventory entry, while `hostvars | list` omits it. A group name is not a host —
      `'num' in hostvars` is False for a group and the read fails identically.

      Two false-positive sources found by the corpus gate rather than by review, both now
      pinned by tests:
      - `hostvars[groups['x'][0]]`, the idiomatic "first host of a group". The quoted
        literal belongs to the *inner* lookup and names a group. This one shape was **20 of
        the 21** hits the rule first produced, every one working code.
      - a `hostvars['x']` written inside a `#` comment — 2 more of the 23 literal uses.
      Both are filtered in `condition::hostvars_host_uses` rather than in
      `hostvars_host_keys`, deliberately: the link-painting caller wants any host name it
      can find, and an ERROR needs the name to be the whole of what was written.

      Corpus: **0 hits** (re-checked at 755 files). `demo/ansible.cfg` now names `inventory-lab.yml` so the demo can
      exercise anything inventory-dependent at all; the full suite is green with it.
- [x] `var-undefined` gains **no new hit** on the corpus from the new sources — measured by
      diffing the two sorted lists, not the two counts. Both sides run against the same
      corpus state (755 files): **707** at `2e86585~1`, before the inventory was a variable
      source, and **238** today. Zero lines appear that were not there before; 469
      disappeared.

      Diff the lists, never the counts. `~/app` is a live working repo — it went from 753 to
      755 files mid-session and the count moved 233 → 238 with no change to this code at all,
      so a bare number recorded here rots within the day. "Zero new lines" is the part that
      stays checkable.

      Reworded, because the box used to read "stays zero-hit on the corpus", which the scan
      flatly contradicts — it prints 233 — and which was never the achievable claim: an extra
      variable source can only move this number *down*, so the thing worth gating is that it
      never moves up. Ticked in `7c944a2` with no measurement recorded, which is how the wrong
      words survived. Re-measured after box 8: unchanged by it, and the new `unknown-host`
      rule is itself 0-hit on the same corpus.
