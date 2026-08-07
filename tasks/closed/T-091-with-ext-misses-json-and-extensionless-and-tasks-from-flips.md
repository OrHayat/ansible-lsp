# T-091 — with_ext misses .json and extensionless, and tasks_from flips the order

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | S    | T-090 | —          |

## Symptom

A role file Ansible loads is reported missing, or we jump to one Ansible would not have
loaded. `tasks_from: setup` against `roles/r/tasks/setup` (no extension) or `tasks/setup.json`
resolves for Ansible and not for us. Where both `tasks/main.yml` and `tasks/main.yaml` exist
only the `.yml` is live, and we do not say so.

## Cause

`resolve.rs:663-671` (`with_ext`) tries `.yml` then `.yaml`. `Role._load_role_yaml` hardcodes
`['.yml', '.yaml', '.json']` (`role/__init__.py:421-422`) — deliberately *not*
`C.YAML_FILENAME_EXTENSIONS`, "to maintain portability" — and then:

- default entry point (`main`): appends `''` **last** → `main.yml`, `main.yaml`, `main.json`, `main`
- with any `*_from:`: inserts `''` **first** (`:429-431`) → the literal name given wins

`DataLoader.find_vars_files` breaks on the first hit (`parsing/dataloader.py:491`), so the
loser is silently dead rather than merged.

## Fix

`RoleExts(&'static [&'static str])` carries the list, with `Default` = `.yml`, `.yaml`,
`.json` — the only list production uses. `RoleExts::candidates(dir, stem, bare_first)`
replaces the old free `with_ext`; `bare_first` says which end the extensionless form goes on:
`false` for the `tasks/main` probe (`ReferenceKind::Role`), `true` for
`ReferenceKind::TasksFrom` — the only `*_from` modelled today (the rest are T-063, and they
take the same flag when they land).

A value rather than a `const` on purpose: it lets a test hold the wrong list beside the right
one, so the pre-fix behaviour is pinned by the suite instead of by whoever remembers to revert
the code. It is deliberately not reachable from config — Ansible hardcodes this list "to
maintain portability", and a knob here would be inventing a feature Ansible doesn't have.

`candidates` allocates once per candidate and once for the vec: the suffix is appended into
the `PathBuf`'s own buffer via `as_mut_os_string`, sized up front, so the old per-candidate
throwaway `format!` string is gone.

Dropped with it: the old short-circuit that returned a lone candidate when the stem already
ended in `.yml`/`.yaml`. Ansible appends the suffixes regardless (`setup.yml.json` really is
probed), and with `''` first the literal always wins before those are reached — so the trail
now says what was actually tried, and `tasks_from: setup.json` no longer probes only
`setup.json.yml`/`.yaml` and misses.

Fixtures: `demo/roles/entrypoints/` holds all four spellings plus a `.jamil` file that is
not a role extension, and `demo/tasks/role_entrypoints.yml` references each one — pinned by
`demo_role_entry_points_resolve_as_documented`. It also pins the case that only the flip
explains: `tasks_from: legacy` cannot reach `tasks/legacy.jamil`, but `tasks_from:
legacy.jamil` can, because the literal name lands in the extensionless slot.

The pre-fix behaviour is asserted, not remembered: `role_file_extension_order_flips_with_tasks_from`
probes one fixture with `RoleExts(&[".yml", ".yaml"])` and with `RoleExts::default()`, and
pins that only the second finds `data.json` — plus both `bare_first` ends under each list, so
the flip is shown to be the only thing deciding the `setup` pair. A hand-revert first
confirmed the two end-to-end tests catch the regression (the demo one fails on
`tasks_from: report` trying only `report.yml`/`report.yaml`); **no other test in the suite
noticed that revert**, which is how the behaviour shipped wrong in the first place.

Nothing was needed for first-hit-wins: `from_candidates` already takes one target, and a
directory is not a hit either — roles pass `allow_dir=False`, which skips a matching directory
and keeps probing (`dataloader.py:484-488`), exactly what `Fs::is_file` does.

## Done when

- [x] `.json` and the extensionless form resolve
- [x] `tasks_from: setup` prefers `tasks/setup` over `tasks/setup.yml`
- [x] a test pins both orders against one fixture tree —
      `role_file_extension_order_flips_with_tasks_from`, one tempdir role holding
      `main.yml`, `main.yaml`, `setup`, `setup.yml`, `data.json` plus a second role with a
      bare `main`. It asserts the full candidate list at both ends, not just the target.
- [x] first-hit-wins is modelled, so a shadowed `main.yaml` is not reported as live —
      the same test pins `targets == [tasks/main.yml]`
