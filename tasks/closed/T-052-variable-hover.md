# T-052 — Variable hover: where it's defined, and its value

| Status | Priority | Size | Epic  | Depends on   |
| ------ | -------- | ---- | ----- | ------------ |
| done   | P2       | S    | T-112 | T-048, T-049 |

## Problem

Go-to-definition (T-050) jumps you to a variable's definition, but you have to leave the file
to see it. A hover should answer "what is this and where does it come from" in place — the
common question when reading someone else's playbook.

## Approach

On hover over a `VarUse`, look the name up in `vars::definitions` and render a tooltip:

- **One definition** → source + location + value, e.g.
  `provisioner_user` — role default (`roles/provisioner/defaults/main.yml`) = `deploy`.
- **Several** → list them in precedence order, so the multi-definition case (which go-to-def
  already surfaces as multiple targets) becomes readable:
  ```
  base_url — 2 definitions
  - play vars = http://localhost
  - set_fact  = https://prod.example.com   (later; wins at runtime for tasks after it)
  ```
- **None** → no hover (same silence as go-to-def), unless T-051 has something to say.

Reuse the `VarSource` labels; keep values literal (no Jinja evaluation — that's the profile
work, T-035/T-052-value). A value that is itself templated is shown verbatim.

## Traps / limits

- Don't assert which definition "wins" beyond the honest note — precedence is host- and
  `-e`-dependent (the 22 levels), which we don't resolve.
- Hover fires a lot; if reading `definitions` per hover is slow, share the cache T-050's paint
  pass will want.

## Done when

- [x] hovering a variable shows its source, file and value
- [x] multiple definitions are listed, not collapsed to one
- [x] cross-file definitions (role defaults, vars_files) render their file
- [x] no hover for names with no reachable definition

Resolution: commit `c2bd855`. Value shown for value-span sources; `set_fact`/`register`
show source only (their span is the name, not the value).
