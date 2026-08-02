# T-065 — `var-undefined`: raise the bar to workspace-wide absence (the 656 fix)

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P1       | M    | T-051      |

## Problem

`var-undefined` shipped with the bar "no definition **reachable from this file**" and its
first honest corpus run produced **656 hits** — disqualifying under T-051's own rule
("if the count explodes, the rule doesn't ship").

Evidence, checked by grep (2026-08-02): the flagged names are overwhelmingly **defined** —
in the corpus's project-root `group_vars/all.yml` (`ftp_cluster_image_name:234`,
`container_temp_dir:133`, `lustre_mount_point:792`, …), which sits next to the *inventory*,
not next to any playbook, so the reachability walk never reads it. That's the
inventory-adjacent gap tracked as T-062. The initial diagnosis in T-051 ("vars live in the
role's defaults") was wrong; this ticket records the corrected one.

## Approach

Interim, honest bar until reachability catches up: flag a use only when its name is
**defined nowhere in the entire workspace**.

- Build one workspace-wide defined-name set: every file's `vars:`/`set_fact`/`register`
  via `index()`, every role's `defaults/`/`vars/` (main files and dirs), all
  `group_vars`/`host_vars` wherever they sit, vars files. Names only, no spans needed.
- `scan`: build once per run. LSP: build at workspace-scan time, invalidate like the
  definitions cache.
- `undefined_uses` consults it as one more exemption, after the existing list.

Costs, accepted deliberately:

- Weaker: a name defined in one unrelated corner shields a genuinely-missing use
  elsewhere. That precision returns where it's decidable — T-059 (call sites), T-062
  (inventory sources), T-060 (guarded near-miss).
- The demo's `env` in `playbook.yml` goes silent (defined in a sibling playbook) — its
  comment must be updated; `region` and `from_inventory` keep firing (defined nowhere).

When T-062 lands, revisit: with inventory-adjacent sources indexed, the reachability bar
may become tenable again — measure, don't assume.

## Done when

- [ ] workspace-wide set built and consulted; reachability logic untouched otherwise
- [ ] corpus gate re-run: count in the tens or lower, **every surviving hit inspected and
      recorded here** as real
- [ ] demo: `region` and `from_inventory` still fire; `env` comment updated for the new
      silence
- [ ] pinned tests: defined-in-unrelated-file → silent; defined-nowhere → still flagged
- [ ] T-051's corrected gate box ticks with a link here
