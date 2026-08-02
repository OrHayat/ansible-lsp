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

Costs — **proposed, not yet user-approved**, and possibly avoidable:

- Weaker: a name defined in one unrelated corner shields a *runtime-true* undefined use
  elsewhere. The demo's `env` in `playbook.yml` is exactly that: defined only in a
  sibling playbook's play vars, genuinely undefined when `playbook.yml` runs — today's
  check warns correctly; this bar would silence it.
- **Try T-062 first.** The 656 are dominated by the inventory-adjacent `group_vars` gap;
  indexing those keeps the reachability bar (so `env` keeps its true warning) and
  silences the false positives for the right reason. If the residual count after T-062
  is small, this ticket closes unimplemented. Only if it stays high does the blunt bar —
  and its precision cost — get decided, by the user, on the measured numbers.

## Done when

- [ ] workspace-wide set built and consulted; reachability logic untouched otherwise
- [ ] corpus gate re-run: count in the tens or lower, **every surviving hit inspected and
      recorded here** as real
- [ ] demo: `region` and `from_inventory` still fire; `env` comment updated for the new
      silence
- [ ] pinned tests: defined-in-unrelated-file → silent; defined-nowhere → still flagged
- [ ] T-051's corrected gate box ticks with a link here
