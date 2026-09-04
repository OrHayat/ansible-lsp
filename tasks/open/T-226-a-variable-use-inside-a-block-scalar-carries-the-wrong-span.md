# T-226 — A variable use inside a block scalar carries the wrong span, so every consumer points at the wrong line

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-112 | —          |

## Symptom

A `{{ name }}` on the fourth line of a `content: |` block is reported two lines up. Measured
with `scan` on this file:

```yaml
- hosts: all
  tasks:
    - name: t
      copy:
        dest: /tmp/x
        content: |
          line one
          line two
          line three
          value={{ zzz_missing }}      # line 10
```

```
UNDEFINED VARIABLES (1):
  p.yml:8  zzz_missing
```

The drift grows with depth: on the reference tree, `spark-setup.yml` reports `host` at line
123 (`autopurge.purgeInterval=1`) for a use on line 126, and `nfs-ganesha-setup.yml` reports
it at 480 for a use on 486 — a shell script in a block scalar, ten spaces of indentation per
line. Every line of a block scalar is short by its stripped indentation, and the error
accumulates.

The scan prints the same span `main.rs` puts in the `var-undefined` diagnostic, and the hover
and go-to-definition read the same `VarUse.span`, so the squiggle, the tooltip position and
the jump are all offset in the editor. **Not yet measured per consumer** — the scan is; the
three editor surfaces are inferred from the shared field and need a test each (rule 1).

## Cause

`vars::template_uses` (`vars.rs:366`) takes the scalar's *value* text and a `base` offset,
and returns `base + offset-into-value`. For a plain or quoted scalar the value is a contiguous
slice of the document, so that is exact. For a block scalar the value is the document text
with each line's indentation removed, so the offset into the value falls short of the
document offset by the indentation of every line before the use. T-049 measured spans as
"byte-accurate" on flow scalars and never had a block-scalar row.

## Fix

Map value offsets back through the block scalar's line structure: for a use at value offset
`o`, add the stripped indentation of each value line that starts at or before `o` (plus any
folded-newline difference for `>` scalars). The parser knows the scalar style (T-162 wants it
exposed anyway) and the indentation indicator; a per-scalar `Vec<(value_offset, doc_offset)>`
line table is enough and is built once per scalar.

Check `condition.rs` and the reference extractor for the same `base +` assumption — a
`when:` is never a block scalar in practice, but a `shell: |` with `{{ }}` in it is the common
case, and that is exactly the shape above.

## Done when

- [ ] the Symptom file reports `zzz_missing` at line 10, with a control on a flow scalar that
      already reported the right line and still does
- [ ] `|`, `>`, `|-`, `|+` and an explicit indentation indicator each have a case, and a use
      on the first line of the block as well as a later one
- [ ] hover, go-to-definition and the `var-undefined` diagnostic each land on the use in the
      editor — one test per consumer, each seen red without the fix
- [ ] the reference tree's `spark-setup.yml:123` and `nfs-ganesha-setup.yml:480` hits move to
      the lines the uses are on
