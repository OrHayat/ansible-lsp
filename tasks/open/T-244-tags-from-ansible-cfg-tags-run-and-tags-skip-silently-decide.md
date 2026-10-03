# T-244 — Tags from ansible.cfg: tags_run and tags_skip silently decide which tasks run

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | S    | T-099 | —          |

## Problem

`[tags] run` and `[tags] skip` in `ansible.cfg` set the default `--tags` / `--skip-tags` for
every play. We read `ansible.cfg` and never read this section, so our picture of which tasks
run is the unfiltered one.

Measured on ansible-core 2.21.2, three tasks — one tagged `a`, one tagged `b`, one untagged:

| ansible.cfg | runs |
| --- | --- |
| (control) no `[tags]` section | `TAGGED-a`, `TAGGED-b`, `UNTAGGED` |
| `[tags] run = a` | **`TAGGED-a` only** |
| `[tags] skip = b` | `TAGGED-a`, `UNTAGGED` |

The `run = a` row is the sharp one: the **untagged** task disappears too. `--tags` is not
"also run these", it is "run only these", so a single line in `ansible.cfg` removes most of a
playbook from the run — with no skip line in the output, because the tasks are filtered before
execution rather than skipped during it.

| setting | env | ini |
| --- | --- | --- |
| `TAGS_RUN` | `ANSIBLE_RUN_TAGS` | `[tags] run` |
| `TAGS_SKIP` | `ANSIBLE_SKIP_TAGS` | `[tags] skip` |

## Filed as a task, not a bug — and what would make it a bug

Nothing we ship today is known to say something false because of this. We do not currently
claim "this task runs", so a filtered-out task is coverage we lack rather than a lie we tell.

That is an **unverified** boundary, not a cleared one. The consumers to measure before
trusting it, each of which reasons about reachability:

- `unused-file` / `unused-role` (T-021) — a file reached only by a task the tag filter removes
- handler reachability (T-196's "provably closed" handler set)
- `notify:` resolution (T-028), which walks the same task set

If any of those answers from the unfiltered task list while `[tags] run` is set, it is making a
reachability claim that the configured run contradicts, and this becomes a P1 bug. Measure
before building — the T-192 lesson is that the hazard being real does not make the rule
shippable.

## Approach

Read `[tags] run` / `[tags] skip` and their env hooks into the config model beside the settings
`config.rs` already carries, then decide per consumer whether the filtered or unfiltered task
set is the right question. Both are legitimate: "what does this playbook contain" and "what
does this configured run execute" are different questions and some rules want each.

Found by the T-144 `base.yml` audit. T-230 covered tags on a dynamic include and is closed;
it never looked at the cfg defaults, so no open ticket owned these two.

## Done when

- [ ] `[tags] run` / `[tags] skip` and `ANSIBLE_RUN_TAGS` / `ANSIBLE_SKIP_TAGS` are read, with
      the env-over-ini precedence asserted
- [ ] the three measured rows above are pinned as a test, including the untagged-task row
- [ ] each reachability consumer listed above is measured under `[tags] run` and recorded here
      as correct-today or re-filed as a bug
