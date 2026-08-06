# T-082 — Hover markdown is assembled by hand, in eight places

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| done   | P3       | M    | —          |

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

- [x] one type owns markdown syntax; no `*`, `` ` ``, `\n\n` or `[](…)` literals left in the eight
      producer functions
- [x] values interpolated from user files are escaped, pinned by a fixture whose `when:` contains
      `*` and a backtick
- [x] output is byte-identical to today's for a fixed set of cursor positions, except where
      escaping is the correction
- [x] hover tests assert on content, not on punctuation — the `/` vs `\` Windows failures in
      `hover_labels_workspace_library_modules_legacy` and
      `hover_lists_glob_targets_for_unknown_value_templated_paths` go with them

## Outcome

`crates/ansible-lsp/src/md.rs`, ~150 lines and no dependency.

**The type is the fix, not the tidying.** A builder whose methods take `String` would have moved
the asterisks into one file and left the bug reachable — the first draft here did exactly that,
and `format!("runs unless {var} is set")` still compiled. So `Md::line` takes an `Inline`, and the
only constructors are `raw` (authored prose), `text` (escaped), `code` (fenced) and `link` (label
escaped). `raw` takes **`&'static str`**: a value read from a user's file is a `String` and never
`'static`, so it cannot be emitted as prose by accident. Forgetting to escape stops compiling
rather than shipping a wrong tooltip.

`Prose for &'static str` gives `"Tried:".bold()` — deliberately not implemented for `&str` or
`String`, or the guarantee would evaporate through deref.

`code()` widens its fence past any backtick run inside the value, which is the actual output bug:
`when: msg | regex_search("`a`*b*")` ended its span early and dumped the rest as prose. `text()`
escapes `_` only at a word boundary — CommonMark disallows intraword `_` emphasis, and Ansible
variable names are mostly underscores, so escaping them all would mark up nearly every condition
label to prevent nothing.

Three findings the ticket didn't have:

1. **`message_for` is not a hover.** It feeds `Diagnostic.message`, which LSP defines as plain
   text, so its backticks are a quoting convention the editor prints literally and markdown never
   renders. It is listed among the eight; it is seven. Left alone, with a comment saying why. The
   same applies to the `when-import-var-mutated` message.
2. **The Windows premise is stale.** Both named tests were verified passing at HEAD on Windows —
   `posix_display` had already fixed the `/` vs `\` failures. The item was done for its own sake:
   a `plain()` helper strips inline markdown so a provenance test doesn't fail over emphasis,
   while checks where punctuation *is* the behaviour (a path is linked, a block is present) still
   read raw markdown.
3. **A real Windows failure sits next door**, untouched by this: `ansible-core`'s
   `nested_task_file_resolves_against_role_tasks_dir` panics on `std::env::var("HOME").unwrap()`
   (`workspace.rs:251`) — Windows has `USERPROFILE`. That is T-077's territory.

Byte-identity was verified by the twelve existing hover tests, which assert exact punctuation and
passed unchanged. One caught a real regression mid-refactor — a dropped "possible" in
`N possible targets:` — which is precisely the guard the ticket asked for, so the assertions were
only loosened afterwards.
