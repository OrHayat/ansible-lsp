# T-237 — The Python package's bundled collections are searched before ~/.ansible/collections, so hover and jump show a copy Ansible does not run

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-118 | —          |

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

## Fix

Configured paths first, bundled last. The unmeasured edges below decide how far past a
reorder this goes — measure each before writing its assertion.

## Done when

- [ ] with a collection in both a configured root and the bundled dir, the resolver picks the
      configured root's copy — fixture with a fake install, no real Ansible needed
- [ ] `ANSIBLE_COLLECTIONS_SCAN_SYS_PATH=False`: measured, and the bundled dir is dropped if
      Ansible drops it
- [ ] `collections_path` set in `ansible.cfg`: measured whether `~/.ansible/collections` is
      still searched (the tool's install roots add it regardless today)
- [ ] the playbook-adjacent `collections/` dir's position relative to both: measured and
      pinned
- [ ] re-run the table in Symptom against the rebuilt binary; all three rows agree with
      Ansible
