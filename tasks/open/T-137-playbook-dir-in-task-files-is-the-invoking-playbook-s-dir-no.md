# T-137 — playbook_dir in task files is the invoking playbook's dir, not a guess

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-113 | T-020      |

## Problem

`expand_magic` substitutes `{{ playbook_dir }}` with two guesses — `<project_root>` and
`<project_root>/playbooks` — and accepts a hit on either. For a reference written **in a
playbook file** that is now exact (T-095 narrowed it to `ctx.file_dir`). Everywhere else —
task files, handlers, roles — the guess remains, and it is a guess about a value that
genuinely varies per invocation.

Two ways it can be wrong:

- **Wrong file, reported confidently.** If `<root>/x.yml` and `<root>/playbooks/x.yml` both
  exist, `{{ playbook_dir }}/x.yml` resolves to the first tried. Go-to-definition opens a
  file Ansible would never load, and it is marked Resolved.
- **False warning.** A path that only exists under the real invoking playbook's directory
  resolves under neither guess, so it is reported missing.

The same disease `role_path` (T-068) and `inventory_dir` (T-070) were disabled for. This one
was kept because the guess was calibrated against the real corpus and the four `~/app/ansible`
references that depend on it stayed navigable — see
`playbook_dir_tries_every_plausible_location`, which resolves from
`roles/lustre-storage/tasks/main.yml` via `{{ playbook_dir }}/../roles/common/...`.

## What the value actually is

Verified from source and by live runs (ansible-core 2.21.2), recorded in `tasks/README.md`:

| Hop | Where | What |
| --- | ----- | ---- |
| 1 | `vars/manager.py:455` | `playbook_dir = self._loader.get_basedir()` |
| 2 | `dataloader.py:224-230` | `get_basedir()` returns the mutable `_basedir` |
| 3 | `playbook/__init__.py:58-62` | parsing a playbook sets `_basedir` to that file's dirname |
| 4 | `playbook_include.py:124-125` | each imported play records `_included_path = dirname(playbook)`, guarded by `if ... is None` so the innermost — the file the play is written in — wins |
| 5 | `playbook_executor.py:115-119` | per play at run time: `set_basedir(play._included_path or pb._basedir)` |

**A play sees the directory of the file it is written in.** A task file has no
`_included_path` of its own, so it runs under its play's basedir — the *invoking playbook's*
directory. A role called from three playbooks in three directories has three values.

Origin: `ffdba96668` (Cammarata, 2015-09-29), fixing ansible#12524 — a 1.9.3→2.0.0 regression
where `_basedir` leaked between sibling included playbooks, so `varnish/main.yml` looked for
its template in `memcached/templates/`. The `is None` guard was in that first commit. This is
defended behaviour, not an accident.

## Approach

Same walk, same index, same memoization as **T-068** — build one and build both. T-068 derives
`role_path` per invocation chain from the reverse index (T-020); this derives `playbook_dir`
the same way, with playbook entry points as the walk roots instead of role-invocation sites.

- `{{ playbook_dir }}` expands to the **set** of chain-derived playbook dirs (usually size 1).
- A hit on any counts, as today — but the set is derived rather than guessed, so a miss
  becomes a warning that cannot lie.
- Files no chain reaches keep globbing and never warn: absence of chains proves nothing,
  because a templated include makes edges unknowable.

Until then the two guesses stay exactly as they are. Removing them without the index would
cost the four navigable references above and buy nothing — globbing cannot handle the `../`
in those paths.

## Watch out

- **T-068:29 cites "the `playbook_dir` policy"** as the model to copy ("try each, a hit on
  any counts"). Corrected there: the *policy* is fine, the *input set* is what needs to stop
  being guessed. Do not reproduce the guessing.
- `T-034` lists `playbook_dir` under "already handled". True for playbook files since T-095,
  not for the rest.
- The corpus (`~/app/ansible`) was absent when this was filed, so the four-reference claim
  comes from the existing test's own comment, not a fresh scan. Re-run before changing
  anything.

## Done when

- [ ] `playbook_dir` in a task file expands to chain-derived dirs, not `<root>`/`<root>/playbooks`
- [ ] a file reached by two playbooks in different directories yields both, pinned by fixture
- [ ] a reference that resolves under a guess but under no real chain no longer resolves
- [ ] files no chain reaches stay silent
- [ ] the four `~/app/ansible` references stay navigable, verified by a corpus scan
