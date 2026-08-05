# T-018 — `meta/main.yml` dependencies

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| done   | P2       | S    | —          |

## Problem

**Only 3 real dependency entries exist in the whole repo**, one each in:

```
roles/docker-network/meta/main.yml
roles/grafana-docker/meta/main.yml
roles/dashboard-docker/meta/main.yml
```

22 other roles declare `dependencies:` but the list is **empty** — the boilerplate
`ansible-galaxy init` template. (An earlier grep counted 39 `dependencies:` lines and was
almost entirely wrong: most hits are unrelated, like a `ha_test_dependencies` variable, an
`echo` string in a shell task, and `roles/sync-state/vars/main.yml` which has its own
`dependencies:` keys inside a data structure. Only 23 of 74 roles have a `meta/main.yml` at
all.)

So the navigation payoff is 3 references. **This is not worth doing for navigation.**

## Why it's still P2

A role dependency *runs before the role does*, so it's a real edge in the reference graph. A
role reached **only** as a dependency has no other inbound edge — and T-021 would then fade it
as unused when it genuinely runs. Three false "unused" hints is enough to discredit that
feature.

So this is a prerequisite for T-021's correctness, not a feature in its own right. It's also
~20 lines, since it reuses T-004's role resolver unchanged.

## Approach

`ReferenceKind::RoleDependency` over `meta/main.yml`, both entry forms:

```yaml
dependencies:
  - podman                      # bare string
  - role: podman                # dict, may carry vars
    vars: { ... }
```

Empty lists must produce no references and no diagnostics — 22 roles have them.

## Done when

- [x] the 3 real dependency entries resolve and navigate (corpus role count 381 -> 384)
- [x] `dependencies: []` produces nothing (pinned, with the galaxy_info-only case)
- [x] both bare-string and `role:` dict forms work (`name:` accepted too; demo fixture has
      the dict form)
- [x] dependency edges are in the reference graph, so T-021 counts them as uses
- [x] **grown mid-flight, correctly:** a dependency's defaults/vars/set_facts are in scope
      for the depending role *and its callers* — the definitions walk follows meta deps
      transitively (cycle-guarded by the visited set), pinned by a two-hop test. Without
      this, `var-undefined` would false-flag vars a dependency provides.
