# T-248 — A role include written ../x.yml loads x.yml beside the file, through Ansible's parent-include retry

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | M    | T-090 | —          |

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

The resolver answers a role file's include once per **way into the file**, read from the
reverse index (`reverse::Chain`, `ReverseIndex::chains`, `resolve::through_chains`). A way in
is the list of `<cum>` folders its includes wrote, nearest first; an entry point — `roles:`,
`tasks_from`, the role's own `main.yml` — has none. Each `<cum>` adds
`<anchor>/<cum>/<anchor name>/<src>` then `<anchor>/<cum>/<src>`. An include searches the
role's own folders first and then the way's; an import searches the way's first.

Ways in that cannot be known — a templated or absolute includer, an includer outside the
role's `tasks/` or `handlers/`, a cycle, more than 64 ways — keep the file-local search for
the whole file, as does a file nothing reaches. Several ways in: all to one file resolves;
all missing is a miss; some missing is a miss whose message names the failing way ("when
this file is reached from `site.yml:5`"); several files resolves to all of them.

`scan` builds the include edges in a first pass (`reverse::ways_in`); the editor holds role
files with includes back from its workspace pass and publishes them from a second one, and
an open buffer, hover and go-to-definition read the live index.

Not modelled, each needing a file where nobody writes one: past an `include_role` that is
itself below an include, Ansible keeps walking with `role/<cum>` and the outer include's
folder; and the `$CWD` candidates (`upstream/ansible-cwd-relative-include.md`). One level
deep: an include that resolves only through a way in adds its edge in the second pass, too
late for the files it reaches in turn. After an includer is edited, the files it includes
keep their answer until T-012.

## Decision (2026-10-21)

The exact rule, through the reverse index, not the `../`-only special case. Measured on
2.21.3 while settling it (`scratchpad/t248/probe/probe.py`, one marker task per candidate):

| Shape                                                               | Ansible loads             | We said before |
| ------------------------------------------------------------------- | ------------------------- | -------------- |
| `main` → `sub/a.yml` → `deep/b.yml` → `../x.yml`, only `sub/x.yml`  | `tasks/sub/x.yml`         | Missing        |
| `sub/a.yml` imports `b.yml`; `tasks/b.yml`, `tasks/sub/b.yml` exist | `tasks/sub/b.yml`         | `tasks/b.yml`  |
| same with `include_tasks`                                           | `tasks/b.yml`             | right          |
| `tasks_from: sub/a` includes `b.yml`, only `tasks/sub/b.yml`        | fails                     | Resolved       |
| playbook includes `roles/r/tasks/a.yml`, with `../x.yml`            | `roles/r/x.yml`, no retry | right          |
| `a.yml` reached by `roles:` and by `tasks_from`, `../x.yml`         | loads, then fails         | Missing        |

Neither of the two silent wrong answers occurs in the 44 public repos or the private corpus.
The variable walk follows includes without the ways in, but measured, it still sees a
`set_fact` in a file only the retry loads (`var-undefined` stays silent, and fires in the
control with no definition), so it needs no follow-up.

## Done when

- [x] cases 1, 2, 3 and 6 resolve to the file Ansible loads, each pinned by a test
- [x] case 4 resolves to `tasks/sub/x.yml`, not `tasks/x.yml`
- [x] cases 5, 7 and 8 keep today's answer, pinned as controls
- [x] a demo row, label pinned by a test
- [x] corpus scans diffed: the `ovirt.ovirt` include resolves, nothing else changes
