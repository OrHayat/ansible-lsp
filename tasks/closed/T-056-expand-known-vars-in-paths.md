# T-056 — Expand known-value variables in templated paths (navigation only)

| Status | Priority | Size | Epic  | Depends on   |
| ------ | -------- | ---- | ----- | ------------ |
| done   | P2       | M    | T-112 | T-048, T-034 |

## Problem

`{{ env }}.yml` in a reference position (e.g. `include_vars`, `template: src:`) is treated as
a runtime unknown — globbed, never navigable to the real file. But when `env` has a
statically-knowable value, the target *is* knowable:

```yaml
vars:
  env: prod
tasks:
  - include_vars: "{{ env }}.yml"   # this loads prod.yml
```

**T-034 deliberately excluded this** — its rule was "`vars:`/`set_fact` literals look knowable
but sit under 22 precedence levels; inventory or `-e` can override them, so expanding one
invents false certainty." That reasoning was correct *for asserting a value*. The variable
model (T-048) changes the calculus: we now know where `env` is defined, its value, its
precedence, and whether it's host-dependent — so we can **offer candidates** without
asserting, the same way templated globs already do.

## Approach

Extend `expand_magic` (the `role_path`/`playbook_dir` mechanism) to also substitute user
variables — but only for **navigation**, never for warnings:

- Look up the `{{ var }}` in `vars::definitions`. If it has one or more **known-literal**
  values (value-span sources: play `vars:`, `group_vars/all`, `vars_files`, role
  `defaults`/`vars`), substitute each as a candidate path.
- Feed the candidates to the existing resolver. Mark the result **`SkipReason::Templated`**,
  so it is navigable (Cmd+click offers the files) but **never warned about** — sidestepping
  T-034's "false certainty" objection entirely: a candidate is an offer, not an assertion.

This is the key line: T-034 refused to *assert* a value; this ticket only *offers* one, which
is exactly what templated paths already do.

## Traps / limits

- **Several definitions → several candidates** (`prod.yml`, `staging.yml`); offer all.
- **Host-dependent** (`host_vars`, named `group_vars`), fact-derived, `-e`, or itself
  templated → value not known → leave it fully templated (glob), don't substitute.
- **`set_fact` stores the name span, not the value** — needs a small value-span addition
  before its literals can be read; play vars / group_vars carry values already.
- **Never emit a missing-file warning** from an expanded user var — `-e` could override it.
  Navigation only. (A literal `loop:` from T-034 *can* warn; a user var cannot.)
- Respect ordering/`in_effect_at` and precedence when choosing which definitions' values to
  offer.

## Done when

- [x] `{{ var }}` with a single known-literal definition resolves its path for navigation
      (`resolve::resolve_with` + `substitute_literals`)
- [x] multiple literal definitions offer one candidate each (cartesian over token values)
- [x] host-dependent / fact / `-e` / templated values stay globbed, not substituted
      (`vars::known_literals` takes value-span, host-independent sources only; a bare
      identifier token only)
- [x] expanded user-var paths are navigable but never warned about (`SkipReason::Templated`,
      Missing → Skipped)
- [x] corpus gate: `scan` uses plain `resolve`, so warnings are unchanged; the LSP path uses
      `resolve_with`

Resolution: `resolve_with`/`substitute_literals` in resolve.rs, `known_literals` in vars.rs,
wired through `analyze_text`. demo: `include_vars_demo.yml` (`env: prod` → `vars/prod.yml`).
Follow-up: `set_fact` values would need a value span (currently name-only), and non-path kinds
aren't substituted.
