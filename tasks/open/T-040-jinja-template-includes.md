# T-040 — Jinja `{% include %}` / `{% import %}` / `{% extends %}` in templates

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P2       | L    | T-120 | T-188      |

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

**All of it** — the whole document grammar, not a reader for four tags, because Ansible parses
the whole document grammar for every template it renders.

`_engine.py:293` picks between two compile paths, and they need different amounts of Jinja:

| path       | entry                            | invokes                  | grammar |
| ---------- | -------------------------------- | ------------------------ | ------- |
| expression | `_compile_expression` (`:379`)   | `env.compile_expression` | `Parser(state="variable")`, `parse_expression()`, then `if not parser.stream.eos: raise "chunk after expression"` |
| template   | `_compile_template` (`:367,375`) | `env.from_string`        | document lexer + every statement form |

`when:`, `loop:` and `assert.that` take the first (`playbook/task.py:615`,
`executor/task_executor.py:178`, `plugins/action/assert.py:81`), and that path physically
cannot reach a statement — which is why **T-188 is complete without one**. A `.j2` file takes
the second: `plugins/action/template.py:102` reads it, `:138` templates it. So the division of
labour between these two tickets is not subset-and-superset of one grammar; it is Ansible's own
two entry points.

Read from `jinja2` 3.1.6 (`parser.py`, `lexer.py`) and **run** against the installed copy. The
ansible-core lines are read from the 2.21.3 sdist and **not run** — no core is installed on the
machine this was written on, so by rule 1 they are the weaker kind of claim and want
re-measuring before anything ships on them.

1. **`parse_expression`** — the four target-naming statements read their target with the *same*
   call: `parse_extends`, `parse_include`, `parse_import` and `parse_from` each do
   `node.template = self.parse_expression()`. This is **T-188's deliverable** and the one part
   of the grammar this ticket does not build.
2. **The lexer — all five states**, from `self.rules` in `lexer.py`: `root`, `comment_begin`,
   `block_begin`, `variable_begin`, `raw_begin`. This is the expensive half and it has no
   partial version, because every way of hiding or faking a delimiter is lexical rather than
   syntactic:
   - `{% raw %}` is a lexer *state*, not a parsed node — which is why an `{% include %}` inside
     one comes out as `TemplateData` and is correctly not a reference (measured; jinja's own
     `test_raw1` pins the render side)
   - `{# ... #}` is its own state
   - `balancing_stack`: a `%}` inside `(`, `[` or `{` is not a terminator
   - neither is a `%}` inside a string — `{% if x == '%}' %}` is one tag, not two
   - whitespace control (`{%-`, `-%}`, `+%}`, `trim_blocks`, `lstrip_blocks`) moves where the
     `data` boundaries fall, so it affects reported spans rather than which target is read
3. **All 14 statement forms.** `_statement_keywords` (`parser.py`) holds 12 — `for` `if` `block`
   `extends` `print` `macro` `include` `from` `import` `set` `with` `autoescape` — plus `call`
   and `filter`, which `parse_statement` dispatches separately (`parser.py:167-190`). Anything
   else is `fail_unknown_tag`.

   Four of them name a template file and are this ticket's product, each a tag name, then
   `parse_expression()`, then modifiers:
   `extends EXPR` · `include EXPR [ignore missing] [with|without context]` ·
   `import EXPR as NAME [with|without context]` ·
   `from EXPR import a[, b as c] [with|without context]`

   The other ten are parsed rather than skipped to their terminator, and `parse_statements`'
   end-token bookkeeping (`_tag_stack`) is parsed with them.
4. **Why all fourteen and not those four.** A reader that walks past unmodelled tags cannot
   tell *a tag I chose not to model* from *a tag that does not exist*, so it has to accept both
   — and can therefore never say a template will not render. Measured on jinja2 3.1.6:
   `env.parse` rejects every one of these, and a four-tag skimmer takes all nine silently.

   | template                          | jinja2 3.1.6                                     |
   | --------------------------------- | ------------------------------------------------ |
   | `{% for x in xs %}{{ x }}`        | unexpected end of template, looking for `endfor`  |
   | `{% if a %}x{% endfor %}`         | unknown tag `endfor`, innermost block is `if`     |
   | `{% forr x in xs %}{% endforr %}` | unknown tag `forr`                                |
   | `{% include 'a.j2' %}{% endif %}` | unknown tag `endif`                               |
   | `{% macro m(a,) %}{% endmacro %}` | expected token `name`, got `)`                    |
   | `{% set x = %}`                   | expected an expression                            |
   | `{% for x in %}{% endfor %}`      | expected an expression                            |
   | `{% raw %}{% include 'x.j2' %}`   | missing end of raw directive                      |
   | `{{ x }`                          | unexpected `}`                                    |

   Rows 6 and 7 are broken *inside tags the four-tag version does not model*, so no amount of
   care in the include reader reaches them. Ansible does not reach them either until the task
   runs on the target — a template is never parsed at playbook-parse time — which is what makes
   this diagnostic worth having rather than a restatement of something the runtime already says
   in time to help.

Box 2 below still falls straight out of step 1: "a templated include name stays silent" is
`Const` versus anything else. Upstream already decided it the same way —
`jinja2.meta.find_referenced_templates` returns `None` for a dynamic name and returns exactly
this ticket's answer for the rest, which makes it a ready-made oracle to test against.

## Traps / limits

- **Search path is call-site-dependent:** a `.j2` reachable from two roles has two resolution
  contexts (each role's `templates/`). One template → possibly several valid resolutions →
  may need the "candidates" UX (T-029 style), not a single jump.
- Dynamic include names (`{% include some_var %}`) are unresolvable — stay silent.
- **The delimiters are not fixed**, so a hard-coded `{%` lexer is wrong twice over. The
  `template:` module takes `variable_start_string` and friends as parameters
  (`plugins/action/template.py:125-134`), and a template may open with a `#jinja2:` header line
  setting any field of `TemplateOverrides` (`_jinja_bits.py:76,163-188`) — including
  `line_statement_prefix`, which switches on the sixth lexer state this ticket otherwise skips.
- **Fourteen is a floor, not a ceiling.** `DEFAULT_JINJA2_EXTENSIONS` (`config/base.yml:839`)
  lets a user register Jinja extensions, and each one adds tags that `parse_statement` resolves
  through `self.extensions`. The unknown-tag diagnostic must not fire when extensions are
  configured. It defaults to `[]` and is deprecated as of 2.21.3, so the default case is safe.
- This is a second grammar (Jinja) over a second file type, and it stays the biggest cost here.
  T-188 pays for the expression half; the document lexer and the statement forms are this
  ticket's, and the lexer is the part that is fiddly rather than long.

## Done when

- [ ] literal `{% include/import/from/extends %}` targets resolve inside `.j2` files
- [ ] a missing include warns; a templated include name stays silent
- [ ] multi-context templates offer all candidate resolutions rather than guessing one
- [ ] a `.j2` include-chain fixture is pinned
- [ ] all 14 statement forms parse, with `{% raw %}` and `{# #}` handled in the lexer, and the
      `%}`-in-a-string and `%}`-inside-brackets cases asserted
- [ ] a `.j2` that will not render is diagnosed, with the nine measured rows above as the test,
      and silent when `DEFAULT_JINJA2_EXTENSIONS` is non-empty
- [ ] overridden delimiters are honoured — both the module parameters and a `#jinja2:` header
- [ ] a differential gate against `jinja2.meta.find_referenced_templates` over the `.j2` files
      of the pinned corpus trees, in the T-184 shape: env-gated, `#[ignore]`d, printing every
      hit, with a `demo/` control that must come back non-zero

Docs: https://jinja.palletsprojects.com/en/latest/templates/#import ·
https://docs.ansible.com/ansible/latest/playbook_guide/playbook_pathing.html
