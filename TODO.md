# TODO

Working branch: `parser-libyaml-poc`. Full backlog lives in `tasks/open/`; this file is the
short list of what's next, most actionable first.

## Variable model — leftovers

- [ ] **Cache eviction.** The variable cache (main.rs) never shrinks — fine for a session,
      unbounded over a long-lived server. Add a size/LRU bound if it ever matters.
- [ ] **T-051 — never-defined diagnostic.** The other half of definedness: warn when a
      variable is used but defined *nowhere* reachable. The condition-aware coverage half is
      done (`var-uncovered-when`); this is the riskier base case — must suppress magic vars,
      `ansible_*`, loop vars, caller-injected, and concede inventory/`-e`.
- [ ] **T-054 — find variable references** (reverse of go-to-def) + role params. Role params
      are the caller-injected case T-053 deferred; they need this "who includes this" index.
- [ ] **T-046 — harden module/args split.** `find_action` is best-effort; do the real
      `ModuleArgsParser` handling (free-form `command: echo hi`, `action:`/`local_action:`,
      `args:` merge). Needed so variable *uses* in module args are exact.

## Cleanup

- [ ] Close **T-016** (`vars_files`) and **T-017** (`include_vars`) — both implemented by
      T-048 / T-053, still sitting in `tasks/open/`. Move to `tasks/closed/`.

## Notes

- Restarting the server after a build: `Ansible LSP: Restart` (Windows) or rebuild in WSL.
- `scan` uses plain `resolve` (not `resolve_with`), so the corpus warning gate is stable.
- Demo files for the variable features: `demo/tasks/variables.yml`, `demo/cross_file_vars.yml`,
  `demo/multi_host_vars.yml`, `demo/conditional_register.yml`, `demo/include_vars_demo.yml`.

## Broader backlog (see tasks/open/)

Execution treeview (T-024), evaluate-when-under-profile (T-035), vault awareness (T-037),
file lookups (T-038), requirements↔collections cross-check (T-039), reverse index (T-020),
unused-file hints (T-021), circular includes (T-022), neovim client (T-026), package .vsix
(T-019), and more.
