# T-237 — The Python package's bundled collections are searched before ~/.ansible/collections, so hover and jump show a copy Ansible does not run

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | M    | T-118 | —          |

## Symptom

A collection installed in both places — `ansible-galaxy collection install` into
`~/.ansible/collections`, and the copy the `ansible` pip/Homebrew package bundles next to
ansible-core in `site-packages/ansible_collections` — resolves to the bundled copy. Ansible
runs the other one.

Measured 2026-09-17, Homebrew `ansible` 14.3.1 (core 2.21.3), `community.general` 9.0.0 in
`~/.ansible/collections` and 13.3.0 bundled, a play with a bare `ufw:` task:

| run                                                      | module file used                                   |
| -------------------------------------------------------- | -------------------------------------------------- |
| `ansible-playbook -vvv`, default settings                | `~/.ansible/collections/.../general/plugins/modules/ufw.py` |
| same, `ANSIBLE_COLLECTIONS_PATH=/nonexistent` (control)  | `site-packages/ansible_collections/.../ufw.py`     |
| our resolver (`AnsibleInstall::detect(None)`, same play) | `site-packages/ansible_collections/.../ufw.py`     |

The control is what makes this an ordering bug and not a detection one: the same Ansible
falls through to the bundled copy only once `~/.ansible/collections` is out of the path. The
tool detected that same install — its `package_dir` matched `ansible --version`.

Hover and go-to-definition therefore show 13.3.0 source for a task that runs 9.0.0. Any
answer read out of the collection (argument specs, routing, action-plugin twins) comes from
the wrong version too.

## Cause

`AnsibleInstall::from_filesystem` (`install.rs`) pushes the bundled
`site-packages/ansible_collections` into `collection_roots` inside each of its three package
branches, and only afterwards appends `ANSIBLE_COLLECTIONS_PATH`, `$ANSIBLE_HOME/collections`
and `/usr/share/ansible/collections`. First match wins downstream, so the bundled copy
shadows.

Ansible does the opposite: `_AnsibleCollectionFinder.__init__`
(`utils/collection_loader/_collection_finder.py:186-200`) takes the configured
`COLLECTIONS_PATHS` first and extends with `sys.path` afterwards, only when
`COLLECTIONS_SCAN_SYS_PATH` is true (default).

## How Ansible picks a copy — read, then measured

**Not by version.** One ordered list of roots; the first root holding `ns/coll` is the only one
ever read. Read from 2.21.3:

1. `<playbook dir>/collections` for each playbook on the command line — prepended:
   `_n_collection_paths = _n_playbook_paths + _n_configured_paths`
   (`_collection_finder.py:280`, set from `cli/playbook.py:119-128`).
2. `COLLECTIONS_PATHS` in written order — `ANSIBLE_COLLECTIONS_PATH`, else the `collections_path`
   ini key, else the default `{{ ANSIBLE_HOME ~ "/collections:/usr/share/ansible/collections" }}`
   (`config/base.yml:287-302`; passed in at `plugins/loader.py:1729`). A set value *replaces*
   the default.
3. Every `sys.path` entry of the Python running Ansible, when `COLLECTIONS_SCAN_SYS_PATH` is true
   (default; env `ANSIBLE_COLLECTIONS_SCAN_SYS_PATH`, ini `collections_scan_sys_path`,
   `base.yml:278-286`). The package's own `site-packages` is one of them.

A root is kept only if it has an `ansible_collections/` subdir, duplicates dropped, and a root
already ending in `ansible_collections` has that stripped (`:202-211`). The pick itself is
`_AnsibleCollectionPkgLoader._validate_final` — "only search within the first collection we
found" (`:700-701`).

Measured 2026-09-17 on 2.21.3 with a marker `community.general.ufw` in each root,
`ANSIBLE_HOME` relocated into scratch so the real `~/.ansible` is untouched, reading
`-vvv`'s "Using module file":

| #  | roots holding the collection                                   | Ansible used        |
| -- | -------------------------------------------------------------- | ------------------- |
| 1  | playbook-adjacent, `$ANSIBLE_HOME/collections`, bundled         | playbook-adjacent   |
| 2  | `$ANSIBLE_HOME/collections`, bundled (control for 1)            | `$ANSIBLE_HOME`     |
| 3  | `ANSIBLE_COLLECTIONS_PATH`=dir without it, `$ANSIBLE_HOME`, bundled | bundled — the default was replaced |
| 4  | `ANSIBLE_COLLECTIONS_PATH`=dir with it, `$ANSIBLE_HOME`, bundled | `COLLECTIONS_PATH`  |
| 5  | bundled only                                                   | bundled             |
| 6  | bundled only, `ANSIBLE_COLLECTIONS_SCAN_SYS_PATH=False`         | not found           |
| 7  | playbook-adjacent, `ANSIBLE_COLLECTIONS_PATH` with it           | playbook-adjacent   |
| 8  | ini `collections_path`=dir without it, `$ANSIBLE_HOME`, bundled | bundled             |
| 9  | bundled only, ini `collections_scan_sys_path = False`          | not found           |
| 10 | `collections/` beside `ansible.cfg`, playbook in `playbooks/`   | bundled — root `collections/` not searched |
| 11 | 10 plus `playbooks/collections/` (control)                     | `playbooks/collections` |
| 12 | `ANSIBLE_COLLECTIONS_PATH` ending in `ansible_collections`      | that root           |
| 13 | `ANSIBLE_COLLECTIONS_PATH` one level too deep (control)         | bundled — ignored   |

What the tool did, besides the order: it added `~/.ansible/collections` even when
`collections_path` replaces it (rows 3, 8); it had no `collections_scan_sys_path` (6, 9); and
the default roots lived in install detection, which reads only the env half of `ANSIBLE_HOME`
and none of `ansible.cfg`.

Row 10 is T-096's problem, not this ticket's: the playbook-adjacent root is the *playbook's*
directory, and we stand the project root in for it everywhere. It stays the stand-in here, in
the position row 1 measured.

Also not modelled: `sys.path` beyond the package's own `site-packages`. Homebrew's Python has
nine entries; knowing them means running Python, which detection avoids on purpose (T-084).

## Fix

`FileContext::collection_roots` builds Ansible's list: the playbook-dir stand-in, then
`collections_path` or its default from the config's `ansible_home`, then the package's
`site-packages` when `collections_scan_sys_path` allows. Install detection keeps only what it
alone can see — that `site-packages` root.

## Done when

- [x] with a collection in both a `COLLECTIONS_PATHS` root and the bundled dir, the resolver picks
      the `COLLECTIONS_PATHS` copy — in-memory fixture, no real Ansible
      (`a_collection_in_several_roots_resolves_to_the_one_ansible_reads_first`, row 2)
- [x] a set `collections_path` (ini or env) replaces `$ANSIBLE_HOME/collections` and
      `/usr/share/ansible/collections` instead of adding to them (rows 3, 8 —
      `a_set_collections_path_replaces_the_default_roots`)
- [x] `collections_scan_sys_path = False` (ini or env) drops the bundled root (rows 6, 9 —
      `scan_sys_path_off_drops_the_bundled_collections`,
      `collections_scan_sys_path_reads_both_spellings`)
- [x] the playbook-dir stand-in comes before `COLLECTIONS_PATHS` (rows 1, 7)
- [x] a root written with a trailing `ansible_collections` is not doubled (row 12, with row 13 as
      its control)
- [x] re-run the table in Symptom against the real install; all three rows agree with Ansible —
      the resolver now picks `~/.ansible/collections/.../ufw.py`, the file `-vvv` names

Each box was seen red by breaking its own line of `collection_roots` or `config.rs`; one break
first failed to compile and was redone before it counted.

## Closing notes

- **Without an install, the default roots are now read.** They moved from install detection to
  the project's config, so `~/.ansible/collections` resolves with no Ansible found; only the
  bundled root needs one. T-133's no-install collection hover said "no Ansible install was found
  to look for it elsewhere", which stopped being true, and now reads "is not in any collections
  path, and no Ansible install was found to check the collections it bundles". Its short-name
  sibling said "builtins and installed collections cannot be looked up" and now says "builtins
  and core's rename table", the two things that do need the install.
- **A test that passed by accident.** `a_reference_we_did_not_follow_says_why` discovered its
  context from the real environment, so once `~/.ansible/collections` became a root it read this
  machine's `community.general`. It runs against an empty environment now.
- **`ansible --version`'s "ansible collection location" line is no longer read.** It reported
  the configured roots as seen from wherever the server started, not the project; the config
  layer answers that per project.
