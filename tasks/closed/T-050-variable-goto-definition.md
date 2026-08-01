# T-050 — Go-to-definition (and link colour) for variables

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| done   | P1       | M    | T-048, T-049 |

## Problem

The first shippable variable feature: Cmd+click a variable and jump to where it's defined.

## Approach

- `goto_definition` falls back to variables when the cursor isn't on a file/role/module
  reference: find the `VarUse` under the cursor, look it up in `vars::definitions`, and return
  a `Location` per definition — resolving each def's line/col in *its own* file (current file
  from the in-memory buffer, others from disk).
- `ansible/references` also emits variable-use ranges (tagged `kind: "variable"`), and the
  client paints them a distinct **violet** (`#B388FF`) vs teal references.
- Stays silent when a name has no reachable definition — inventory/`-e`/caller vars are not
  indexed, so absence is not "undefined".

`demo/tasks/variables.yml` (in-file, incl. block/task vars and a two-definition case) and
`demo/cross_file_vars.yml` (+ vars/role fixtures) demonstrate it.

## Done when

- [x] Cmd+click / F12 on a variable jumps to its definition(s), in-file and cross-file
- [x] multiple definitions offered, not collapsed
- [x] variable links coloured distinctly and underlined (in-file and cross-file)
- [x] no jump / no paint for names with no reachable definition

Resolution: commits `1206823`, `a0a32a7`, `be77c8b`, `377b17a`.
