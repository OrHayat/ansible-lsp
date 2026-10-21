# T-248 — A role include written ../x.yml loads x.yml beside the file, through Ansible's parent-include retry

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-090 | —          |

## Symptom

Inside a role, a task file that was itself reached through an include or import can write
`include_tasks: ../x.yml` and Ansible loads `x.yml` from **the same folder** as that file. We
read `../` literally, so we report the file missing, or resolve it to a different file than
the one Ansible runs.

Measured on 2.21.3. Every layout is a role `r` run from `site.yml`:

| Case | Layout                                                                  | Ansible loads    | We say                        |
| ---- | ----------------------------------------------------------------------- | ---------------- | ----------------------------- |
| 1    | `main.yml` includes `a.yml`; `a.yml` includes `../x.yml`; `tasks/x.yml`  | `tasks/x.yml`    | **Missing**                   |
| 2    | same, with `import_tasks: a.yml` and the include inside a `block:`       | `tasks/x.yml`    | **Missing**                   |
| 3    | `main.yml` imports `sub/a.yml`; it includes `../x.yml`; only `tasks/sub/x.yml` | `tasks/sub/x.yml` | **Missing**             |
| 4    | as 3, with both `tasks/x.yml` and `tasks/sub/x.yml`                      | `tasks/sub/x.yml` | **Resolved, to `tasks/x.yml`** |
| 5    | as 3, with only `tasks/x.yml`                                            | `tasks/x.yml`    | Resolved — right              |
| 6    | handler `include_tasks: a.yml`; `handlers/a.yml` includes `../x.yml`; `handlers/x.yml` | `handlers/x.yml` | **Missing**    |
| 7    | as 6, with `x.yml` only in `tasks/`                                      | fails            | Missing — right               |
| 8    | control: `../x.yml` written directly in `tasks/main.yml`, no parent      | fails            | Missing — right               |

Case 4's target is by elimination, not read off a surface: case 3 shows we never reach
`tasks/sub/x.yml`, and `tasks/x.yml` is the only other `x.yml` in the fixture.

Found in real code: one include in the public `ovirt.ovirt` collection. It was the only false
`Missing` on a task include across 45 public repos (~7,000 task includes) plus the private
corpus, 2026-10-20.

## Cause

`playbook/included_file.py:136-180`. When the first lookup misses, Ansible walks the
include's parents. Every `TaskInclude` counts, and `import_tasks` is one, so a static import
qualifies. For each parent it takes `dirname` of the parent's raw path, accumulates it up the
chain as `cumulative_path`, and in a role tries two candidates:

```python
new_basedir = os.path.join(role_path, dirname, cumulative_path)   # dirname: 'tasks' | 'handlers'
candidates = [
    loader.path_dwim_relative(role_path,   dirname, include_target, is_role=True),
    loader.path_dwim_relative(new_basedir, dirname, include_target, is_role=True),
]
```

The second call passes a folder that already ends in `tasks/<cumulative_path>`, and
`path_dwim_relative` joins `dirname` onto it a second time. Traced for case 3, every disk probe
of that second call, in order:

```
roles/r/tasks/sub/tasks/../x.yml   no    raw join, tasks/sub/tasks does not exist
roles/r/tasks/sub/x.yml            yes   the same join after unfrackpath: '..' ate the doubled 'tasks'
```

For case 5, the same call goes on to `unfrackpath(join(basedir, source))` =
`tasks/sub/../x.yml` = `tasks/x.yml`, the literal reading. It comes after the doubled one,
which is why case 4 picks `sub/x.yml`. If neither hits, the walk moves to the next parent up.

The doubled `dirname` looks like an upstream accident: nothing documents it, and
`playbook_pathing.html` lists no such step. There is no dossier yet.

## Fix

For a file in a role's `tasks/` or `handlers/` reached through an include or import, add the
candidate `<role>/<dir>/<cum>/<src>`, normalised, ahead of the literal `<file_dir>/<src>`.
`<dir>` is `handlers` for a handler, `tasks` otherwise. `<cum>` is the accumulated `dirname`
of the parent include paths, so this needs the reverse index (T-020): which include loads this
file, and what it wrote.

The candidate only differs from the literal one when `src` starts with `../`. For any other
`src` it adds `<role>/<dir>/<cum>/tasks/<src>`. That is a real Ansible candidate too, but
measure it before adding it, since it widens.

A file loaded by several parents with different paths has several `<cum>`s. Answer it the
way T-096's open decision answers several invoking playbooks; until that is decided, add the
candidate only when every parent agrees.

## needs decision: how does the resolver learn how a file was reached?

Measured 2026-10-21 on 2.21.3: the same `a.yml` with `include_tasks: ../x.yml` and
`tasks/x.yml` present **fails** when `a.yml` is entered as `include_role: {name: r,
tasks_from: a}`, and likewise with `import_role`. An `IncludeRole` parent contributes the role
path itself as `cumulative_path` (`included_file.py:145-146`), so its retry repeats the first
lookup. Whether `../x.yml` loads therefore depends on **how** the file was reached, not on
where it sits.

The resolver works one file at a time and never reads the reverse index; only the LSP's
consumers do (`main.rs:250`, `:715`). The options:

1. **Feed inbound edges to the resolver.** Exact: an include/import edge gives the parent's
   written path and so `<cum>`, while a `tasks_from` edge rules the retry out. But a file's
   resolution then depends on other files, so the scan must resolve includers first, and the
   LSP must re-resolve a file when an includer changes, which is T-012's invalidation.
2. **File-local guess.** Apply the retry to any role task or handler file except `main.yml`,
   with `<cum>` taken from the file's own subdirectory. Cheap, and right for every corpus case,
   but it says `Resolved` for a `tasks_from` entry, which is measured wrong.
3. **Hedge, no verdict change.** Keep `Missing`, but when the retry candidate exists, lower the
   diagnostic to a hint naming it ("loads `tasks/x.yml` if this file is reached through an
   include"). No index needed, and never `Resolved` for a run that fails. Go-to-definition
   still has no target.

Recommended: 3 now, then 1 when T-012 lands.

## Done when

- [ ] cases 1, 2, 3 and 6 resolve to the file Ansible loads, each pinned by a test
- [ ] case 4 resolves to `tasks/sub/x.yml`, not `tasks/x.yml`
- [ ] cases 5, 7 and 8 keep today's answer, pinned as controls
- [ ] a demo row, label pinned by a test
- [ ] corpus scans diffed: the `ovirt.ovirt` include resolves, nothing else changes
