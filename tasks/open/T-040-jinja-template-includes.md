# T-040 — Jinja `{% include %}` / `{% import %}` / `{% extends %}` in templates

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P2       | L    | T-120 | —          |

## Problem

`.j2` templates are treated as leaves — the LSP resolves `template: src=foo.j2` (T-015) but
stops at the file. Real templates pull in others:

```jinja
{% extends "base.conf.j2" %}
{% include "partials/header.j2" %}
{% import "macros.j2" as m %}
```

Template-heavy repos have deep include chains, and a broken include is invisible until render.

## Approach

Index `.j2` files and extract `include`/`import`/`from`/`extends` targets (literal string
args). Resolve relative to the template's own directory plus the searchpath of the task that
renders it. Go-to-definition inside templates; warn on a missing include.

### What this needs from Jinja's grammar

Read from `jinja2` 3.1.6 (`parser.py`, `lexer.py`). In order:

1. **`parse_expression`** — all four statements read their target with the *same* call:
   `parse_extends`, `parse_include`, `parse_import` and `parse_from` each do
   `node.template = self.parse_expression()`. This is **T-188's deliverable**, so the grammar
   this ticket books as its biggest cost is paid there, not here.
2. **The lexer's `root` state** — scanning document text for a delimiter and emitting the text
   between as `data`. Two rules. T-188 never needs this: its parser starts already inside an
   expression (`Parser(state="variable")`), so it never scans for `{%` at all.
3. **The lexer's `block_begin` state** — the shared `tag_rules` tokeniser (names, strings,
   numbers, operators) plus a `%}` terminator and its `-%}` / `+%}` variants. `variable_begin`
   is the same `tag_rules` with a different terminator, so T-188 pays for the tokeniser and
   this is the terminator on top.
4. **Four statement forms**, each a tag name, `parse_expression()`, then modifiers:
   `extends EXPR` · `include EXPR [ignore missing] [with|without context]` ·
   `import EXPR as NAME [with|without context]` ·
   `from EXPR import a[, b as c] [with|without context]`
5. **`{% raw %}` must be skipped whole.** Its contents are literal text, so an `{% include %}`
   inside one is not a reference. Jinja's own `test_raw1` pins this: a `{% baz %}` inside
   `{% raw %}` renders as the characters `{% baz %}`.

Steps 2, 3 and 5 are what remains once T-188 lands; step 1 is the part this ticket no longer
has to build. Whitespace control (`{%-`, `-%}`, `trim_blocks`, `lstrip_blocks`) moves where the
`data` boundaries fall, so it affects reported spans rather than which target is read.

Box 2 below falls straight out of step 1: "a templated include name stays silent" is
`Expr::Lit` versus anything else, a match on T-188's output rather than a rule to design.

## Traps / limits

- **Search path is call-site-dependent:** a `.j2` reachable from two roles has two resolution
  contexts (each role's `templates/`). One template → possibly several valid resolutions →
  may need the "candidates" UX (T-029 style), not a single jump.
- Dynamic include names (`{% include some_var %}`) are unresolvable — stay silent.
- This is a second grammar (Jinja) over a second file type. That was the biggest cost here
  until T-188; the expression half is now shared, and what is left is the document scan,
  the block terminator and four statement forms.

## Done when

- [ ] literal `{% include/import/from/extends %}` targets resolve inside `.j2` files
- [ ] a missing include warns; a templated include name stays silent
- [ ] multi-context templates offer all candidate resolutions rather than guessing one
- [ ] a `.j2` include-chain fixture is pinned

Docs: https://jinja.palletsprojects.com/en/latest/templates/#import ·
https://docs.ansible.com/ansible/latest/playbook_guide/playbook_pathing.html
