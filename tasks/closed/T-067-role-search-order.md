# T-067 — Role search order doesn't match Ansible's

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| done   | P1       | M    | T-090 | —          |

## Problem

`roles_roots()` (`workspace.rs:55`) adds "the current role's parent" as a roles root for
*every* file. Real Ansible's search for a play-level `roles:` entry
(`definition.py:164-188`) is:

1. `<playbook_dir>/roles`
2. configured `roles_path` (replaces the defaults — we already model that)
3. `role_basedir` — **only** while loading a role's dependencies
4. `<playbook_dir>` itself
5. last resort: the role *name* as a path, relative to the process CWD
   (`definition.py:190-194`)

The parent-as-root rule is an over-approximation of item 3: Ansible has it only in
dependency context, we apply it everywhere. Live-verified consequence: `demo/playbook.yml`'s
`roles: - demo` resolves in our resolver but hard-errors in real Ansible when run from
`demo/` (`The role 'demo' was not found`). From the repo root it resolves — but only via
item 5, the CWD fallback, which no static tool can know. The tool goes silent on a break
Ansible reports, which is this board's definition of P1.

Item 5 is unknowable statically. Policy: a role reachable *only* through it stays
unresolved, and the missing-role diagnostic's candidate list may note the fallback path
that would need a specific launch directory.

## Measured, 2026-09-13, ansible-core 2.21.2

All probes run from `/tmp`. The "live-verified" demo claim above was run from `/mnt/c`, where
WSL makes the directory world-writable and Ansible **ignores `ansible.cfg`** (`config file =
None`), so `roles_path` was never applied in it. D1 below re-runs it on a `/tmp` copy with the
cfg honoured. Probes: `scratchpad/t067_search_order_probe.sh`,
`t067_playbook_dir_root_probe.sh`.

`definition.py:_load_role_path`, read from the installed core:

```python
role_search_paths = [os.path.join(self._loader.get_basedir(), 'roles')]   # 1
if C.DEFAULT_ROLES_PATH: role_search_paths.extend(C.DEFAULT_ROLES_PATH)   # 2
if self._role_basedir:   role_search_paths.append(self._role_basedir)     # 3
role_search_paths.append(self._loader.get_basedir())                      # 4
# then unfrackpath(role_name) — the name as a path, against the CWD        # 5
```

`_role_basedir` is `os.path.dirname(owner._role_path)`, set only in
`RoleMetadata._load_dependencies` (`role/metadata.py:88`).

| Row | Setup | Ran |
| --- | ----- | --- |
| A1 | `dup` in `playbooks/roles` **and** `roles_path` | `playbooks/roles` — item 1 beats item 2 |
| A2 | only in `roles_path` | `roles_path` (control) |
| B1 | `dup2` in `roles_path` **and** `playbooks/` | `roles_path` — item 2 beats item 4 |
| B2 | only in `playbooks/` itself | ran — item 4 is real |
| B3 | removed | not found in `playbooks/roles : nowhere : playbooks` |
| C1 | `a`'s `meta/main.yml` depends on sibling `b`, under a dir no root names | `b` ran |
| C2 | `include_role: b` from inside `a`'s tasks | **not found** |
| C3 | `import_role: b` from inside `a`'s tasks | **not found** |
| C4 | play-level `roles: [.., b]` | not found (control) |
| D1 | demo copied to `/tmp`, `roles: - demo` | `The role 'demo' was not found in: demo/roles:demo/roles:demo:demo` |
| D2 | same copy, `include_role: {name: demo}` | same |

A set `roles_path` replaces the defaults: a role in `~/.ansible/roles` stopped resolving the
moment `roles_path = ./roles` was set (`t067_roles_path_defaults_probe.sh`). The existing
comment was right.

### Divergences, as found

1. **Order.** `roles_path` came first; `<playbook_dir>/roles` must.
2. **Parent-as-root everywhere.** Only meta dependencies get it (C1 vs C2/C3). This is what
   made `roles: - demo` resolve: `find_role` guesses `demo/` is a role (it has `tasks/`), and
   its parent — the repo root — holds a folder called `demo`.
3. **Item 4 missing.** `<playbook_dir>` itself was not a root at all, so a role directory
   beside a playbook was reported missing — a false warning the ticket did not name.

Item 5 is upstream's bug, not a lookup to model: filed as
[#87100](https://github.com/ansible/ansible/issues/87100), dossier
`upstream/ansible-role-name-cwd-fallback.md`. The policy above stands.

## What landed

`FileContext::roles_roots(in_playbook)` now builds Ansible's list: `<playbook_dir>/roles`,
`roles_path` or the defaults, the role's parent **only when the file is that role's
`meta/`** (every dependency is resolved with the meta file's own context, and nothing else
in it is a role reference, so the gate is a property of the file), then `<playbook_dir>`.

`<playbook_dir>` is only knowable in a playbook. In any other file it is whichever playbook ran
it, so `project_root` stands in and item 4 is left out rather than guessed; the file's own
`roles/` stays as the last stand-in so a rootless task file does not start reporting a role
beside it as missing (`role_file_extension_order_flips_with_tasks_from` caught exactly that
when it was dropped). That half is [[T-096]].

| Test | Rows |
| ---- | ---- |
| `a_play_role_searches_playbook_roles_then_roles_path_then_the_playbook_dir` | A1, B1 |
| `a_play_role_beside_the_playbook_resolves` | B2 |
| `a_meta_dependency_finds_a_sibling_role` | C1 |
| `a_role_include_does_not_find_a_sibling_role` | C2, C3 |
| `demo_role_rows_match_their_labels` | D1, D2 — every role row in the demo |

The first four failed before the change for the right reason (wrong copy, `Missing`,
`Resolved`) — C1 passed before and after. Each was confirmed red under a break of its own
piece: item 1 moved after `roles_path`, item 4 removed, the meta gate forced true, forced
false. The demo test went red with the gate forced true.

### The demo

Two sets of demo rows were false labels, not just the one the ticket named:
`playbook.yml`'s `- demo` / `- role: demo`, and `tasks/main.yml`'s "ROLES (work now)" section,
three `include_role: name: demo` rows that Ansible also rejects (D2). Those now use the real
`roles/notifier`; `playbook.yml` keeps one `- demo` as a labelled **BAD** row that warns.
"Role with no tasks/main.yml — silent" never showed that case — `demo/` has a `main.yml` — so
it now points at a new `roles/tasks-from-only`, which has none, and the scan lists it under
ROLES WITH NO tasks/main.yml for the first time.

Demo scan: the only new warning is that BAD row. The var walk fell from 1402 edges to 296: it
had been walking the whole demo tree as the tasks of a role named `demo`. No undefined-variable
verdict changed.

## Done when

- [x] play-level role references search exactly Ansible's list (1, 2, 4), pinned by fixture
      — in a playbook; elsewhere the playbook dir is unknown and stays [[T-096]]'s
- [x] parent-as-root survives only in meta-dependency resolution, pinned by fixture
- [x] `roles: - demo` in the demo warns missing-role, matching the live run (D1, from `/tmp`
      with the cfg honoured) — a labelled BAD row, pinned by `demo_role_rows_match_their_labels`
- [x] a live `ansible-playbook` run per root verifies the order (A1–D2). Nothing contradicted
      a current test; one existing comment (a set `roles_path` replaces the defaults) was
      re-measured and holds

Source: `~/ansible_source/lib/ansible/playbook/role/definition.py:131-198`
