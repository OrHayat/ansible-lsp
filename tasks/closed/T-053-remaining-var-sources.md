# T-053 — Remaining variable-definition sources: include_vars and role params

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| done   | P2       | M    | T-112 | T-048      |

## Problem

`vars::definitions` (T-048 Phase 2) covers role `defaults/`+`vars/`, `vars_files:`, and the
`set_fact`/`register`/`vars:` in included files. Two statically-knowable sources are still
missing, so variables from them don't resolve:

- **`include_vars:`** (precedence 18) — `include_vars: x.yml`, `{ file: x.yml }`, and the
  directory form `{ dir: vars/ }` which loads every file in a folder. Related: T-017.
- **Role parameters** (20, 21) — `include_role`/`import_role`/`roles:` entries carrying
  `vars:`, and a play/role passing vars into what it includes. These define names *for the
  callee*, the mirror image of the "caller-injected" gap noted in T-051.

## Approach

- `include_vars` file form: resolve the path like `vars_files` (reuse `resolve_var_path`), read
  the flat map. Dir form: resolve the directory, index every `*.yml` in it — check the
  directory exists, never a particular file.
- Role params: when following a role/include reference, carry its `vars:` block as definitions
  scoped to the collected callee — each key a `VarSource::RoleParam` with the span at the call
  site.

## Traps / limits

- `include_vars` is a task, so it can be templated (`file: "{{ os }}.yml"`) — glob/skip, don't
  warn, same as templated includes.
- The `dir:` form defines *many* names; navigate to the directory, and for definedness (T-051)
  treat every name it could load as defined.
- Keep it deterministic — still no folder scanning for group_vars/host_vars.

## Done when

- [x] `include_vars` file form resolves and its keys are indexed with spans
- [x] `include_vars` dir form indexes every file in the directory
- [x] templated forms are skipped (not guessed)
- [x] a demo example jumps to the new source (`include_vars_demo.yml`)
- [ ] ~~role/include `vars:` params~~ — **deferred**. These are the *caller-injected* case:
      the definition is at the call site, the use is inside the callee, and `definitions()`
      starts at the edited file without walking *up* to callers. Resolving them needs a reverse
      "who includes this" index — folded into T-054. Adding them now would only create defs
      that never match a use.

Resolution: commit for include_vars file + dir forms. Role params tracked under T-054.
