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

- `Located` grows `via: Option<(PathBuf, Span)>` — the file and span of the *edge* that
  pulled the defining file into scope.
- Stamp it only where the route is non-obvious: the meta-dependency block in
  `vars::collect`. Direct `roles:`, `include_tasks`, `vars_files` etc. stay un-stamped —
  the user wrote those lines in the file they're hovering, the route is visible.
- **One hop, nearest edge**: stamp entries added by each dependency's walk with that
  dependency's line in `meta/main.yml`; on transitive chains the inner recursion stamps
  first and outer levels don't overwrite, so a def always points at the meta edge that
  *directly* names its role — the next file to open, not the whole chain.
- Hover renders it as a suffix on the definition line:
  `_(via dependency — [provisioner/meta/main.yml:7](…#L7))_`, clickable like the
  definition link.

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
