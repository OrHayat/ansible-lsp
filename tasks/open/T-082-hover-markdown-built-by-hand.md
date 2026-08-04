# T-082 — Hover markdown is assembled by hand, in eight places

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P3       | M    | —          |

## Problem

Every tooltip the server sends is a `String` built with `format!` and `push_str`, with the
markdown syntax written inline as literals. Eight functions in `main.rs` do this independently:

```
when_hover        message_for      module_hover     tried_list
guard_line        target_line      path_substitution_hover     variable_hover_at
```

There are ~53 inline markdown literals across them — `"**Tried:**"`, `push_str("\n- ")`,
`format!("**→ `{}`**")`, `"\n\n---\n\n"`, `format!("[{label}]({u}#L{line})")`. The structure of a
tooltip (a heading, a list, a divider, a link) exists only as punctuation inside format strings.

Three consequences, all observed rather than hypothesised:

- **Composition is string surgery.** T-078 needed a module's provenance *and* its guard in one
  tooltip. That came out as `format!("{b}\n\n{g}")` at the call site — the joining rule lives
  nowhere, so the next thing that composes two hovers will invent its own `\n\n`.
- **No escaping anywhere.** Values interpolated into these strings are file paths, variable names,
  raw `when:` expressions and module names, all from user files, none escaped. A `when:` containing
  `*` or `_` or a backtick renders wrong — and conditions are echoed verbatim whenever
  `condition::classify` has no label for them, which is the common case.
- **Tests assert on punctuation.** `hover_shows_module_provenance_not_paths` and friends check
  `md.contains("- action plugin:")`, so a formatting change breaks tests that are supposed to be
  about content. Two tests in this crate already fail on Windows purely because they assert on
  `/` versus `\` inside markdown.

## Approach

A small builder that owns the syntax, so the eight producers describe *structure* and never write
a `*` or a `\n\n` themselves. Something on the order of:

```rust
Md::new()
    .bold("Tried:")
    .list(res.candidates.iter().map(|c| Md::code(shorten(c, ctx))))
    .divider()
    .line(guard)
```

What it has to get right, in priority order:

1. **Escaping** — one `code()`/`text()` distinction, so an expression with `*` in it stops
   rendering as emphasis. This is the only part that fixes a real output bug rather than tidying.
2. **Joining** — `\n\n` between blocks, `\n` inside a list, decided once. T-078's `{b}\n\n{g}`
   becomes a `.concat()`.
3. **Links** — `[label](uri#Lline)` built from a `Url` and a line number, not by `format!`.

Explicitly **not** in scope: changing what any hover says. This is a refactor with no user-visible
diff, which is also how it should be verified — snapshot the current output for a fixed set of
cursor positions first, then require it byte-identical afterwards except where escaping corrects
something.

Do not reach for a markdown crate. The output surface here is five constructs wide and a
dependency would be larger than the code it replaces.

## Done when

- [ ] one type owns markdown syntax; no `*`, `` ` ``, `\n\n` or `[](…)` literals left in the eight
      producer functions
- [ ] values interpolated from user files are escaped, pinned by a fixture whose `when:` contains
      `*` and a backtick
- [ ] output is byte-identical to today's for a fixed set of cursor positions, except where
      escaping is the correction
- [ ] hover tests assert on content, not on punctuation — the `/` vs `\` Windows failures in
      `hover_labels_workspace_library_modules_legacy` and
      `hover_lists_glob_targets_for_unknown_value_templated_paths` go with them
