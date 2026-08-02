# T-066 — Hover provenance breadcrumb for non-obvious definition routes

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| closed | P2       | S    | T-018      |

## Problem

Since T-018 the walk follows `meta/main.yml` dependencies, so hover on `{{ network_mtu }}`
in a playbook that names only `provisioner` correctly shows
`role default · network-base/defaults/main.yml:2` — but nothing explains why
`network-base` is in scope at all. The user never wrote that role's name anywhere in the
hovered file. Definitions that arrive through an edge the user can't see from where they
are reading need the route spelled out (user-confirmed 2026-08-02: "like called from
role.... meta.yaml:<line>.... yes").

## Approach

- `Located` grows `via: Vec<(PathBuf, Span)>` — the chain of meta edges that pulled the
  defining file into scope, ordered from the read file outward (mirrors Ansible's own
  `dep_chain`). Shipped first as one-hop `Option`, upgraded to the full chain same day:
  at depth ≥ 2 a single nearest edge points at a file that is itself invisible from the
  hovered file, leaving the rest as detective work.
- Stamp only where the route is non-obvious: each meta-dependency level in
  `vars::collect` prepends its edge to everything its subtree added. Direct `roles:`,
  `include_tasks`, `vars_files` etc. stay un-stamped — the user wrote those lines in the
  file they're hovering, the route is visible.
- Hover renders it as a stack trace under the definition line, innermost first, each hop
  a clickable link (user-picked over a one-line arrow chain, which got unreadable at
  depth ≥ 3):
  `- required by \`chain-e\` — [chain-e/meta/main.yml:3](…#L3)`
- **No length cap**, matching the MAX_DEPTH removal and real Ansible: chain length is
  bounded by the visited set (once per role), cycles can't extend it.
- Demo: `dependency_chain.yml` + `roles/chain-a..f` cover depths 0–5 in one file
  (live-run: all six execute, deps first), plus `chain_included` — a var loaded by an
  `include_vars` task inside chain-c, showing mechanism on the def line and the
  required-by route underneath (live-run verified, pinned in the chain hover test).

## Done when

- [x] `network_mtu` hover in the demo shows the `provisioner/meta/main.yml` breadcrumb
      with the `network-base` line number (pinned against the real demo file:
      `hover_breadcrumbs_meta_dependency_routes_only`, main.rs)
- [x] defs from roles the playbook names directly show no breadcrumb (pinned, vars.rs +
      the demo `provisioner_user` assertion)
- [x] transitive dep (A → B → C): C's defs point at B's meta, not A's (pinned,
      `transitive_dep_via_points_at_nearest_meta_edge`)
- [x] cycle fixture still terminates with `via` stamping on
      (`mutual_meta_dependencies_terminate_and_both_contribute` still green)

## Note — first-route-wins is deliberate (verified 2026-08-02)

A role reachable both directly and as a meta dependency gets its breadcrumb (or lack of
one) from whichever route the walk reaches first. That looked like an order-dependence
bug and a "strip via when also directly named" post-pass was almost added — then killed
by source verification: Ansible compiles *both* copies of the role's tasks
(`Role.compile`, deps-first) and skips the later copy per host at iteration time
(`play_iterator.py` "role has already run"; `allow_duplicates` defaults false for
`roles:`/deps, identity = name+path+params+when+tags via `Role.__eq__`). So the first
route in listed order is the one that actually executes, and the walk's stamping matches
runtime truth in both orders. `include_role`/`import_role` are exempt from the dedup
(task-level `allow_duplicates` defaults **true** and overwrites the role's meta,
`role_include.py:51,88`) — irrelevant here since only meta edges get breadcrumbs.
