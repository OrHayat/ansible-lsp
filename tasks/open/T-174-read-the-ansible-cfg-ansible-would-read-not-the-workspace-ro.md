# T-174 — Read the ansible.cfg ansible would read, not the workspace root's

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P2       | M    | —          |

## Symptom

A repo with more than one `ansible.cfg` gets answers from whichever one sits at the
workspace root — which is not necessarily the one the user's `ansible-playbook` reads. The
config decides `inventory`, `roles_path`, `collections_path` and `library`, so a mismatch
shows up as a hover pointing at the wrong role, a variable resolving to the wrong value, or
a `var-undefined` on a name the run defines.

This repo already has the shape: `demo/ansible.cfg` exists and the workspace root has none.
Open the repo at the root and we read no config at all; `cd demo && ansible-playbook …`
reads one. Nothing in the tool says which it used until T-062's picker started naming the
file.

## Cause

Measured on 2.21.2, `ansible-config dump --only-changed` from three directories with two
configs present:

| cwd                              | `CONFIG_FILE`            | `roles_path` from the parent config |
| -------------------------------- | ------------------------ | ----------------------------------- |
| `parent/`                        | `parent/ansible.cfg`     | applied                             |
| `parent/child/`                  | `parent/child/ansible.cfg` | **not applied**                   |
| `parent/child/grandchild/`       | `None`                   | —                                   |

Three facts, all of them ours to match:

- The file is chosen by the **current working directory**, not by the project.
- It **does not walk up**: a directory with no `ansible.cfg` gets no config, even with one
  in its parent.
- A second config **never merges**. The chosen file wins whole, so an unset key falls back
  to the built-in default rather than to a nearer file's value.

There is a second half, found by checking rather than assuming. The project root is
resolved **per file**, not per workspace — `scan .` on this repo reports two contexts:

```
config: <no project root> -> none found; built-in defaults  (4 files)
config: ./demo            -> ./demo/ansible.cfg             (105 files)
```

`publish_inventory` asks about `<root>/x.yml`, a path that does not exist, which lands in
the first bucket. So the picker and status bar can report **no config** while every file the
user is editing is analysed with one. A workspace-level answer to a per-project-root
question is wrong whenever a repo holds a playbook tree below its root, which is the normal
shape.

`config.rs:158` uses `ANSIBLE_CONFIG` else `project_root.join("ansible.cfg")`. The no-walk-up
half is right by accident; the cwd half is not modelled at all, and ansible's remaining two
rungs — `~/.ansible.cfg` and `/etc/ansible/ansible.cfg` (`manager.py:296-300`) — are missing.

## Fix

Cwd is invisible to an editor for the same reason `-i` is, so the same shape of answer
applies: model the observable rungs, and let the user state the one we cannot see.

- Add the missing rungs: `~/.ansible.cfg`, then `/etc/ansible/ansible.cfg`.
- When the workspace holds more than one `ansible.cfg`, that is a real ambiguity — offer the
  choice the way T-062's picker offers `-i`, defaulting to the root's. Reuse that panel
  rather than inventing a second one.
- Keep naming the settled file wherever the answer is shown. T-062 does this in the picker
  note; hover and the scan report should agree.

Deliberately **not** doing: picking the `ansible.cfg` nearest each playbook. It is a
plausible-looking guess that ansible never makes — no walk-up, measured — and it would
quietly disagree with every run from the repo root.

## Progress 2026-10-21

The ladder now ends the way Ansible's does. `scan` on a project with no `ansible.cfg` names
`~/.ansible.cfg` when one exists (`config: <no project root> -> …/.ansible.cfg`), and still
reports built-in defaults without one. Corpus scans are unchanged, since neither file exists on
the measuring machine. Left: the picker for several `ansible.cfg` files, naming the settled file
on every surface, and the per-file status bar.

## Done when

- [x] `~/.ansible.cfg` and `/etc/ansible/ansible.cfg` are read when nothing nearer exists,
      in that order, pinned by test — `config::fallback_config_file`,
      `home_then_system_config_when_the_project_has_none`. Measured on 2.21.3 with a scratch
      `HOME`; the `/etc` rung is from `manager.py:299`, since writing there needs root
- [x] a second `ansible.cfg` never contributes a key the chosen one leaves unset — the same
      test (project over home), and `env_path_values_anchor_to_the_project_not_the_env_config`
      (`ANSIBLE_CONFIG` over project): one file is read, whole
- [ ] a workspace with two or more `ansible.cfg` files can be told which one to use
- [ ] the settled file is named wherever a config-derived answer is shown
- [ ] the picker and status bar answer for the file being edited, not for a synthetic path
      at the workspace root — a repo whose playbooks live below the root must not be told
      it has no config
- [ ] corpus count does not rise
