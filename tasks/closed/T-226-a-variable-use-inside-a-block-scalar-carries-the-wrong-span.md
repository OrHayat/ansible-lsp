# T-226 — A variable use inside a block scalar carries the wrong span, so every consumer points at the wrong line

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | M    | T-112 | —          |

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

## Resolution

The fix is on the data: `Node::Scalar` carries `lines`, a `(value offset, document offset)`
table per content line of a `|`/`>` scalar, built in `parse_libyaml::block_lines`, and
`Node::doc_offset`/`doc_span` read it. `vars::walk_uses` maps every use's `span` and `expr`
through it, so each reader of a `VarUse` gets the document position. The table is matched
line by line against the value instead of re-implementing folding and chomping. It returns
`None` when a line does not match, and a reader then falls back to `span.start + offset`.

The consumers, each with a test seen red with the table disabled:

| Consumer | Test |
| --- | --- |
| `undefined_uses` / `scan` | `vars::a_use_in_a_block_scalar_is_reported_where_it_is_written` |
| `var-undefined` diagnostic | `var_undefined_in_a_block_scalar_is_anchored_on_the_name` |
| hover | `hover_answers_on_a_use_inside_a_block_scalar` |
| go-to-definition | `definition_answers_on_a_use_inside_a_block_scalar` |
| `looped-register-key` (reads the document at the use's end) | `a_key_read_inside_a_block_scalar_fires_on_the_key` |
| the guard check `handled_in_expression` (reads the document at `expr`) | the guarded row of the first test |

The last one also changed the counts, not just the lines. With the wrong `expr` span the guard
check read unrelated text and dropped 93 unguarded uses on the reference tree: 178 reports
before, 271 after. With the guard check disabled, both builds report the same 988, so all 93
come from that one filter. In a copy of the tree with one of them rewritten as a flow scalar,
the old build reports it too.

Not changed: semantic tokens still skip a scalar whose value is not its source slice
(`yaml_scalar_tokens`), so a block scalar gets no Jinja painting. That is a missing surface,
not a wrong answer, and it could now paint through `doc_span`.

Flow scalars got the same treatment afterwards. `parse_libyaml::flow_lines` decodes escapes,
`''` and line folding, and starts a table entry after each one. The table is kept only if the
decoding reproduces libyaml's value byte for byte. On the reference tree plus `demo/`, 213 of
the 214 flow scalars whose value differs from their source get a table. The exception is a
YAML-tagged scalar in a docs config, which falls back. One new report follows from it
(`prerequisites-install.yml:1023`, a quoted string over two lines). It is the default-chain
false positive filed as T-246, which the same expression on one line already triggered.

## Done when

- [x] the Symptom file reports `zzz_missing` at line 10, with a control on a flow scalar that
      already reported the right line and still does
- [x] `|`, `>`, `|-`, `|+` and an explicit indentation indicator each have a case, and a use
      on the first line of the block as well as a later one
- [x] hover, go-to-definition and the `var-undefined` diagnostic each land on the use in the
      editor — one test per consumer, each seen red without the fix
- [x] the reference tree's `spark-setup.yml:123` and `nfs-ganesha-setup.yml:480` hits move to
      the lines the uses are on. Neither hit exists any more: at `12d9c16e`
      `nfs-ganesha-setup.yml` is gone and `spark-setup.yml` has no `host` use. Checked across
      the whole tree instead: all 271 UNDEFINED hits land on a line containing their name. The
      control, with the table disabled, has 8 of 178 on the wrong line, all of them in block
      scalars.
