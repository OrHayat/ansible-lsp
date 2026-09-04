# T-225 — A configured inventory path that does not exist drops the group_vars and host_vars beside it, which ansible still reads

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | M    | T-112 | —          |

## Symptom

`ansible.cfg` says `inventory = inventory.yml`, the file is generated and not checked in, and
`group_vars/all.yml` sits next to where it would be. On a checkout without the generated file,
every variable defined only in that `group_vars/all.yml` is reported as `var-undefined`, with
no hover and no go-to-definition — the whole T-062 gap, back.

Measured on the reference tree (`volumez/matrix/ansible`, commit `186c7ed5`, 768 files),
`scan` on 2026-09-04:

| `inventory.yml` | `var-undefined` hits |
| --------------- | -------------------- |
| absent (the checkout as it is) | **434** |
| present, as an empty `all: {hosts: {}}` | **197** |

The 237 that vanish are, by grep, names defined in the root `group_vars/all.yml`
(`daos_build_output_dir:841`, `container_temp_dir:345`, `versity_submodule_path:587`,
`cib_batch_env:693`, `pulp_url:1388`, …). T-062's 238 was measured with the file present;
the same tree on a machine without it reads as a rule that regressed.

**Ansible reads that directory whether or not the file exists.** Live on ansible-core 2.21.2:

```
ansible.cfg:            inventory = elsewhere/inventory.yml     (no such file)
elsewhere/group_vars/all.yml:  x: FROM_ELSEWHERE_GROUP_VARS
group_vars/all.yml:            x: FROM_ROOT_GROUP_VARS
plays/p.yml:            - hosts: localhost … debug: msg="x={{ x | default('UNDEFINED') }}"

$ ansible-playbook plays/p.yml
[WARNING]: Unable to parse …/elsewhere/inventory.yml as an inventory source
    "msg": "x=FROM_ELSEWHERE_GROUP_VARS"
```

Three runs, so the answer could have come out otherwise: the file present gives the same
`FROM_ELSEWHERE_GROUP_VARS`; deleting `elsewhere/group_vars/` gives `UNDEFINED`, so the
root and playbook-adjacent copies were never the source, and the cwd was the root
throughout. The directory beside the *configured path* is what is read, and its existence
is what matters, not the inventory file's.

`-i sub/nope.yml` on the command line behaves the same: warning, exit 0, `x` from
`sub/group_vars/all.yml`, and `UNDEFINED` with no `group_vars` beside the missing path. With
`ANSIBLE_INVENTORY_ANY_UNPARSED_IS_FAILED=true` the run stops instead ("Completely failed to
parse inventory source"), which is the one setting that makes a missing inventory fatal —
worth reading when deciding whether the status bar's `missing` entry is a warning or an error.

**Upstream, this is intended**, not a bug to file: `vars/plugins.py:78`
`get_vars_from_inventory_sources` says `# always pass the directory of the inventory source
file` and checks existence only for comma-separated host lists; `inventory/manager.py:225`
`parse_sources` keeps a source that failed to parse in `self._sources` and only warns
(`:344`), then hands that same list to the vars plugins (`:249-251`, and `vars/manager.py:245`
at play time). A missing file is a warning by design, governed by `INVENTORY_UNPARSED_WARNING`
and `INVENTORY_ANY_UNPARSED_IS_FAILED`.

## Cause

`inventory::source_dirs` (`inventory.rs`) returns a directory only for a path that
`is_dir()` or `is_file()`; a missing path contributes nothing, and the comment above
`sources` calls that "the normal state where inventories are generated and untracked". That
is the right rule for `sources` — a missing file has no hosts — and the wrong one for
`source_dirs`, because the vars plugin keys off the path, not the file. `vars.rs:1593`
walks `source_dirs` for `group_vars`/`host_vars`, so the missing file takes its directory
with it.

## Fix

`source_dirs` yields the parent of a configured path that does not exist, when that parent
exists — the same answer as for a file that does. A directory source that is missing has no
basedir to give (measured? no: **measure it** before deciding; a missing `-i dir/` may or may
not read `dir/group_vars`, and the answer goes in the code comment).

Hosts stay as they are: `sources` still drops the missing file, and `unknown-host` still
answers from no hosts — that half is correct.

**And say it, the way Ansible says it.** Today the missing file is visible only in the status
bar's per-folder `missing` list (T-202). Ansible prints a warning on every run, and the tool
should match: a diagnostic on the `inventory =` line of `ansible.cfg` (or, for a setting
that names the file, in the status bar) reading "`inventory.yml` does not exist — ansible
warns *Unable to parse … as an inventory source* and runs with only implicit localhost;
`group_vars/` and `host_vars/` beside it are still read". Severity follows the user's own
config. `config.rs` already reads `ansible.cfg` — that is where `inventory = inventory.yml`
comes from — but only its `[defaults]` section (`config.rs:191` flips `in_defaults` and
nothing else); these three keys live under `[inventory]`, which it skips today:

| setting (`base.yml`) | `ansible.cfg` | env | default | effect on a missing source |
| -------------------- | ------------- | --- | ------- | -------------------------- |
| `INVENTORY_UNPARSED_WARNING` | `[inventory] inventory_unparsed_warning` | `ANSIBLE_INVENTORY_UNPARSED_WARNING` | `True` | the "No inventory was parsed" warning; off silences it |
| `INVENTORY_UNPARSED_IS_FAILED` | `[inventory] unparsed_is_failed` | `ANSIBLE_INVENTORY_UNPARSED_FAILED` | `False` | fatal when *no* source parsed |
| `INVENTORY_ANY_UNPARSED_IS_FAILED` | `[inventory] any_unparsed_is_failed` | `ANSIBLE_INVENTORY_ANY_UNPARSED_IS_FAILED` | `False` | fatal when *any* source fails — measured: "Completely failed to parse inventory source", no play |

WARNING by default; ERROR when either `*_is_failed` is on and the missing source would trip
it (`any` always; `unparsed` only when it is the sole source or every source is missing).
Note the env var for the middle row is `ANSIBLE_INVENTORY_UNPARSED_FAILED`, not
`…_IS_FAILED` — read off `base.yml:1797`, easy to get wrong from the setting name.

Re-measure after: the 434 must become 197 on the tree as checked out, by diffing the two
sorted lists — zero lines may appear that are not in the with-file run.

## Progress

Landed 2026-09-04. `inventory::source_dirs` keeps a missing source's parent — the table on
the function records every measured row, including the one the Fix section asked for: a
missing *directory* source is "not a directory" to `os.path.isdir`, so it gets the file rule
and its parent is read (`-i sub/nope/` → `sub/group_vars`), while an existing empty directory
is its own base and its parent is *not* read. `sources` is untouched, so hosts stay empty.

The status bar says it. `inventory_status` gains `missing`, `missingTier` and `missingNote`
(window level and per folder), built by `Backend::missing_inventory` from the folder's
effective config — setting, env or `ansible.cfg`; a missing `/etc/ansible/hosts` is the auto
state and not reported. `config.rs` now reads the `[inventory]` section for the three
booleans, ini and env, each pinned. The note is Ansible's wording and ends with the fact the
reader would otherwise get wrong: the directory beside the file is still read. Tiers, each
measured on 2.21.2 with `-i sub/nope.yml`: warning and exit 0 by default;
"Completely failed to parse inventory source" and no play under `any_unparsed_is_failed`;
"No inventory was parsed, please check your configuration and options." under
`unparsed_is_failed` when nothing parsed. `ansible.cfg` is not a served document (the
client's selector is ansible/yaml/j2), so the status bar is where it lands; the client picks
the glyph from the tier and prints the server's note.

Per consumer, one test each in `main.rs` — diagnostic, hover, go-to-definition — plus the
status payload, plus `source_dirs` itself in `inventory.rs`. Every one seen red with the old
`is_file` rule restored. **The first fixture did not go red**: its `play.yml` sat beside
`group_vars/`, so the playbook-adjacent read found the file whichever rule `source_dirs`
used. The play now lives in `plays/`, one level down, the way the corpus lays it out, and the
test comment says why.

Corpus, as checked out (no `inventory.yml`), after the fix: **197**, and the sorted list is
identical to the with-file run — zero lines new, zero gone. Full workspace suite green.

## Done when

- [x] `source_dirs` yields the directory beside a missing configured file, pinned by a test
      with the control that a missing *directory* source behaves as measured on core
- [x] the missing-directory case is measured on ansible-core and recorded in the code comment
- [x] `var-undefined`, hover and go-to-definition all answer from `group_vars/all.yml` beside a
      missing `inventory.yml` — one test per consumer, each seen red without the fix
- [x] the reference tree, as checked out, reports the with-file count (197 at `186c7ed5`)
      and the list diff shows no new line
- [x] a missing configured inventory is reported where the user set it, with Ansible's own
      wording, WARNING by default — and the message says `group_vars/` beside it is still read
      (status bar; `ansible.cfg` itself is not a served document, see Progress)
- [x] `config.rs` reads the three `[inventory]` settings above, ini and env, and the severity
      follows them: ERROR under `any_unparsed_is_failed`, ERROR under `unparsed_is_failed`
      only when no source would parse, silent-by-config never (a fatal run is never silenced)
- [x] each severity tier measured against core, not reasoned: one run per row of the table
- [x] T-051's gate note and T-065 are updated with the corrected number
