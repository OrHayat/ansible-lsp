# T-142 — Parser panics on a block scalar with no trailing newline

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | bug  | P1       | S    | —          |

## Symptom

`Document::parse` **panics** — not returns `None` — on valid YAML whose last line sits inside
a block scalar and which does not end in a newline:

```
thread panicked at libyaml-safer-0.3.0/src/scanner.rs:166:21:
unexpected end of input
```

Minimal cases, both `>` and `|`:

| Input | Result |
| --- | --- |
| `x: >\n  abc` | **panic** |
| `x: >\n  abc\n` | ok |
| `- command: \|\n    a b` | **panic** |
| `- command: >` (marker, no content) | ok |
| `a: 1` (no newline, plain scalar) | ok |
| `` (empty) | ok |

Found by the T-077 corpus sweep, in a real shipped file:
`geerlingguy/ansible-for-devops` (MIT) `includes/provisioning/tasks/composer.yml`. It is not
truncated — upstream is byte-identical at 432 bytes — and **PyYAML parses it fine**, returning
3 tasks. Ansible runs it. We crash on it.

This is the worst failure mode we have. `parse()` returns `Option`, so every caller is written
to handle "unparseable" gracefully; a panic goes straight past all of them. In the editor the
buffer is re-parsed on **every keystroke**, and a file whose last line is inside a block scalar
with no trailing newline is entirely ordinary — plenty of editors never add one, and while
typing it is the normal state. So this is reachable constantly, not a corner.

## Cause

`parse_libyaml::events` hands the text to `libyaml-safer`'s scanner, which panics rather than
returning an error when input ends mid-block-scalar. Nothing between the scanner and
`Document::parse` converts that panic into the `None` the signature promises.

The trigger is the *missing final newline*, not the block scalar itself — the same content
with `\n` appended parses. A block-scalar marker with no content is also fine, so it needs at
least one content line.

## Fix

Fixed in the dependency, not by padding our input. Padding was the first idea and was
rejected: it would have injected a character the document does not have, and a trailing
newline is **not** cosmetic for a block scalar — clip chomping keeps one only when the source
has one, so `cmd: >\n  a\n  b` is `"a b"` while the same text with a final newline is
`"a b\n"`. Padding would have silently changed values.

`read_line_break` now returns instead of panicking when the buffer is empty:

```rust
let Some(front) = self.buffer.front().copied() else { return; };
```

That restores the convention the rest of the file already follows — `IS_Z_AT!` is
`buffer.get(i).is_none()` and `CHECK_AT!` is `get() == Some(c)`, so end of input is modelled
as absence and tests false against every character. The `if / else if is_break(front)` chain
below already falls through silently for any non-break char, exactly like libyaml's
`READ_LINE` over its NUL-padded buffer. Only the `None` arm broke that.

Carried by a fork, pinned by commit:

```toml
[patch.crates-io]
libyaml-safer = { git = "https://github.com/OrHayat/libyaml-safer", rev = "b98cc42" }
```

**Still to do: open the PR against `simonask/libyaml-safer` and drop the patch when it lands.**
That repo is active (0.3.0 shipped 2025-12-22, 3 open issues), so it has a fair chance.

Not done, and worth its own ticket: the crate has 89 panic sites (`panic!`, `unwrap`,
`expect`) against 0 `unsafe` blocks — "safer" names the absence of UB, not of aborts. A
language server is fed half-typed buffers on every keystroke, so a `catch_unwind` at the
`Document::parse` boundary is still worth having whatever the parser does.

## Done when

- [x] `x: >\n  abc` and the `|` form parse instead of panicking
- [x] a document with and without a trailing newline produces the same tree and the same spans

      Amended: they must **not** produce the same tree. The values differ by exactly the
      trailing newline, cross-checked against PyYAML on the same input — which is also the
      proof that nothing was padded. The test asserts the four values literally.

- [x] `ansible-for-devops`' `composer.yml` shape is a test fixture, inline

      `parse_libyaml::tests::a_block_scalar_at_end_of_input_parses_instead_of_aborting`, plus
      `tests/test_block_scalar_eof.rs` in the fork.

- [x] the corpus sweep runs over all five downloaded repos without panicking

      3074 files, 0 panics, 0 unparseable.
