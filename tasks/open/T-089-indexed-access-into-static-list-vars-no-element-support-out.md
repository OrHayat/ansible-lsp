# T-089 — Indexed access into static list vars: no element support, out-of-bounds unflagged

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P3       | M    | —          |

## Problem

A list-valued var works only as a whole. Given

```yaml
vars:
  list1: [apple, banana, fig]
```

`{{ list1 }}` hovers, navigates, and passes the undefined-variable check. But element
access is invisible to the extension:

- `{{ list1[0] }}` — hover/goto fire only on the `list1` characters; the `[0]` is dead
  space, and nothing shows *which* element you get. Hover shows the whole flattened list.
- `{{ list1[6] }}` — no bounds check, even though the list is fully static and the index
  is a literal. Live-verified on 2.21.2: this is a **fatal task error** ("object of type
  'list' has no attribute 6"), while `list1[0]`, the attribute form `list1.0`, and the
  negative form `list1[-1]` all run fine. So an out-of-bounds literal index on a
  statically-known list is provably broken code the editor stays silent about.

Root cause: `condition::variable_uses` tokenizes `list1[0]` down to the bare name (the
`[` just ends the token) and nothing anywhere models the subscript. The var index
(`vars.rs`) records a list def as one name with the whole sequence as its value span;
`known_literals` (T-056) captures scalar values only.

## Approach

Three layers, in increasing ambition — each usable alone:

1. **Parse the subscript**: after a variable token, recognise `[<int>]`, `[-<int>]`,
   `['key']`, and the `.<int>` attribute form, attaching an access path to the `VarUse`
   instead of discarding it.
2. **Element hover/goto**: when the effective def is a statically-known literal list
   (AST sequence, no templating) and the index is a literal int, hover shows the element
   value and goto lands on the element's span, not the whole list.
3. **Bounds diagnostic**: literal int index outside `-len..len` on that same
   statically-known list → diagnostic (fatal at runtime, so ERROR by the T-087
   precedent). Gate hard: only when the effective definition is host-independent,
   single, untemplated, and a real AST sequence — same conservatism as T-056
   known-literals; a dynamic or overridable list must stay silent (P3 because of this
   gating, not because the runtime failure is mild).

Quoted string keys (`mydict['key']`) fall out of the same subscript parse and set up the
dict analogue, but dicts are out of scope here.

## Done when

- [ ] hover anywhere on `list1[0]` (including the subscript) shows the element value for
      a statically-known list; goto jumps to the element
- [ ] `list1.0` and `list1[-1]` resolve the same way (live-verified valid forms)
- [ ] a literal out-of-bounds index on a statically-known list gets an ERROR diagnostic;
      `# noqa` suppresses it
- [ ] templated, multiply-defined, host-scoped, or non-literal-index cases stay silent
- [ ] demo gains labeled GOOD/BAD cases and LSP-level tests pin them
