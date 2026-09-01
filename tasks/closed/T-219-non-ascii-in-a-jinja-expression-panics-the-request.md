# T-219 — Non-ASCII in a Jinja expression panics the request

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | S    | T-114 | —          |

## Symptom

A template containing a non-ASCII name crashes, rather than returning a wrong answer:

```
byte index 7 is not a char boundary; it is inside 'é' (bytes 6..8) of `{{ café_port }}`
```

Measured on the shipped `.j2` path, before any of T-217's YAML work — this is not new:

```
"{{ café_port }}"           -> PANIC
"{% if café %}x{% endif %}" -> PANIC
"{# café #}"                -> 1 token   (comments use `find`, never the walker)
"{{ 'ש' }}"                 -> 3 tokens  (`skip_string` jumps the literal wholesale)
```

The last two rows are why this went unnoticed and why a single example proves little: the
panic depends on where the character sits relative to the bracket walker, not on whether the
file contains non-ASCII. Every existing test used ASCII names.

Reachable from `semantic_tokens`, `references_in` and `will_not_render` — everything routed
through `template::document_in`. Any Ansible tree with a non-ASCII variable name, or a
template with a non-ASCII string outside quotes, hits it.

## Cause

`find_end` walks the source one **byte** at a time (`i += 1`, indexing `bytes`), and tested
for the closing delimiter with `self.src[i..].starts_with(end)`. Slicing a `&str` requires a
char boundary, so the moment `i` steps into the middle of a multi-byte character the slice
panics.

Nothing about the test needed a `&str`: a delimiter is ASCII, and a multi-byte character's
continuation bytes can never match its first byte.

## Fix

Compare bytes — `bytes[i..].starts_with(end.as_bytes())`. Same test, no boundary requirement.

The three other `src[i..]` sites in `lexer.rs` and the four in `raw_end` are reached with `i`
already on a boundary (they advance by whole tokens or by ASCII whitespace), and the sweep
below covers them: 42 of its 72 shapes panic without the fix and none panic with it.

## Done when

- [x] `bytes[i..].starts_with(end.as_bytes())` replaces the `str` slice in `find_end`
- [x] a sweep over non-ASCII in expression, filter, attribute, call, tag, raw body, loop
      binding, data, string, unterminated tag, comment and overridden-delimiter positions —
      six scripts including an emoji and a combining mark — asserts no panic
- [x] the sweep is seen failing without the fix, and carries a control that it reaches the
      tokenizer at all rather than passing by not running
- [x] LSP columns proved to be UTF-16 units, since a `Span` is UTF-8 bytes and the two
      diverge on exactly this input — asserted by slicing the line by UTF-16 units at the
      reported column and requiring the token's own text back
