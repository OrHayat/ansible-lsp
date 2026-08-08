# T-142 — Parser panics on a block scalar with no trailing newline

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P1       | S    | —          |

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

Append a newline before scanning when the text does not end in one. A trailing newline is not
significant in YAML — the identical document with and without it parses to the same tree — so
this changes no result, only whether the scanner walks off the end.

Do it in `parse_libyaml::events`, the single place the text reaches libyaml, so every entry
point is covered at once (`parse_lenient`, `duplicate_keys`).

Two things to get right:

- **Spans must stay correct.** Appending a byte at the very end cannot shift any existing
  offset, but check that no span is allowed to *point at* the synthetic newline — a span
  ending at `len` is fine, one starting there is not.
- A panic reaching the LSP is its own hazard. Even with this fixed, consider whether
  `Document::parse` should be panic-safe at the boundary, given it is fed arbitrary
  half-typed buffers.

## Done when

- [ ] `x: >\n  abc` and the `|` form parse instead of panicking
- [ ] a document with and without a trailing newline produces the same tree and the same spans
- [ ] `ansible-for-devops`' `composer.yml` shape is a test fixture, inline
- [ ] the corpus sweep runs over all five downloaded repos without panicking
