# T-040 — Jinja `{% include %}` / `{% import %}` / `{% extends %}` in templates

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P2       | L    | T-120 | T-188      |

## Problem

`.j2` templates are treated as leaves. Real templates pull in others:

```jinja
{% extends "base.conf.j2" %}
{% include "partials/header.j2" %}
{% import "macros.j2" as m %}
```

Template-heavy repos have deep include chains, and a broken include is invisible until render.

Nothing navigates *into* a template today, and nothing navigates *to* one either: `src:` is
T-015's and T-015 is open, so the `src:` values in `demo/templates_chain.yml` are dead. This
ticket said the opposite — "the LSP resolves `template: src=foo.j2` (T-015)" — which was read
off the ticket number rather than run.

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

Read from `jinja2` 3.1.6 (`parser.py`, `lexer.py`) and **run** against the installed copy.

**The ansible-core half has now been run** (2.21.2 under WSL), which it had not been when this
was written. Every claim below held except one, and the nine jinja2 rows re-confirmed verbatim
on 3.1.6:

| claim | measured |
| --- | --- |
| a template is never parsed at playbook-parse time | **confirmed** — a `.j2` containing `{% forr %}` sits in `templates/` and the playbook runs `ok` as long as no task renders it |
| the template path takes the whole document grammar | **confirmed** — rendering it fails with `Syntax error in template: Encountered unknown tag 'forr'`, at task run time |
| an ordinary scalar is the template path | **confirmed** — `msg: "{% for i in [1,2] %}{{ i }}{% endfor %}"` renders `12`, and `{% if %}` likewise |
| `when:` is `compile_expression`, ending in "chunk after expression" | **confirmed verbatim** — `when: "x == 1 chunk"` fails with `Syntax error in expression: chunk after expression`; `when: "x == 1 %}"` gives `unexpected '}'` |
| `#jinja2:` header overrides delimiters | **confirmed by output**, not by task status: `[% name %]` renders and `{{ not_a_var }}` survives as literal text — an undefined name that would otherwise have failed the render |
| `template:` module parameters override delimiters | **confirmed by output**, same control |

**The correction.** "That path physically cannot reach a statement" is true in effect and wrong
about the mechanism. On 2.21.2 `when: "{% if true %}x{% endif %}"` fails with **`Encountered
untrusted template or expression`** — the Data Tagging trust gate, which fires *before* the
expression parser and never reaches "chunk after expression" at all. The conclusion this ticket
draws from it stands: a statement cannot appear in `when:`. But anything built on the stated
reason should note that two different gates enforce it, and only one of them is the parser.

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

## Progress: the document lexer is in

`jinja::template` splits a template into `Data` / `Comment` / `Statement` / `Expression`
blocks. That is step 2 of the three above — the half the ticket calls "expensive" and
"has no partial version". No box below is finished by it, because every one of them also
needs the statement forms or the resolution on top; this is the floor they stand on.

All four lexical traps are asserted, each with a control:

| trap | asserted |
| --- | --- |
| `{% raw %}` is a state | `{% raw %}{% include 'x.j2' %}{% endraw %}` is one `Data` block, and the same include outside a raw *is* a `Statement` |
| `{# … #}` is a state | `{# {% include 'x.j2' %} #}` is one `Comment` |
| `%}` in a string | `{% if x == '%}' %}ok{% endif %}` is three blocks, not four |
| `%}` in brackets | `{{ {'a': 1} }}` and `{{ f(a, [1, 2], {'k': 'v'}) }}` are one block each |

Whitespace control moves the `Data` boundaries, including on `raw`/`endraw`, whose tags emit
no block for a generic pass to work from. Delimiters are a parameter, so both override
mechanisms have somewhere to go.

### Two oracles, because they answer different questions

`env.lex` gives the block split. `env.parse` gives whether the template renders, and it is the
stricter: `'{{ x'` comes back from the lexer as **no tokens and no error**, while `parse` calls
it `unexpected end of template, expected 'end of print statement'` — which is what this port
says. So the asserted invariant is one-directional: **anything we refuse, `env.parse` must also
refuse.** The converse waits for the statement forms, since `{% if x %}` is a lexically
complete tag and an unclosed block.

### Measured, not assumed: `keep_trailing_newline`

Stock jinja2 drops a template's final newline; **Ansible keeps it** — a `.j2` ending
`hello {{ name }}
` renders `hello world
`, verified byte-for-byte with `od`. The corpus
generator therefore runs `Environment(keep_trailing_newline=True)`. Comparing against a default
`Environment` would have made this port "wrong" about a byte Ansible keeps, and the first
corpus run failed on exactly that before it was chased down.

### Gate

`scripts/jinja_blocks.py` generates the goldens. 44 rows are checked in (adversarial plus
jinja2's own `tests/` and `demo/`); the pinned trees go behind `JINJA_TEMPLATE_CORPUS` in the
T-184 shape, since 1571 real templates are 8.6 MB of JSON.

**1565 real `.j2` files from the eight pinned trees split identically to jinja2, 0 differences,
4 refused — and `env.parse` refuses all 4.** Seen red: dropping the string tracking in
`find_end` splits `{% if x == '%}' %}` into `statement " if x == '"` + `data "' %}ok"`, and
both the checked-in corpus and the tree gate fail on it.

## Progress: the statement forms are in

`jinja::statement` reads a `{% … %}` body and `check` walks a whole template with a tag stack.
All nine measured rows are refused and — the control that makes that mean something — the
*repaired* form of each is accepted, so a reader that simply refused everything would not pass.

The four target-naming tags are read through the one `parse_expression` call upstream uses.
Silence is asserted where it is owed: a dynamic `{% include some_var %}` names nothing, and
neither does a reference inside `{% raw %}`, inside `{# #}`, or inside a string.

### Against `find_referenced_templates`, the oracle this ticket named

**1558 real templates from the pinned trees, 54 literal references, 0 differences, and 0
falsely refused** — that last number is the one that matters, because a false "this will not
render" is the worst answer this feature can give.

Two things the corpus found that reading the grammar had not:

- **`{% include ['a.j2', 'b.j2'] %}`** names *both*, tried in order. A list is not a dynamic
  name, and upstream reports both; the first version reported neither.
- **`{% set x %}…{% endset %}`** is real. `set` is standalone with a top-level `=` and a block
  without one, and treating it as always-standalone reported a live kubespray template as
  broken. Depth matters in that test: the `=` in `{% set x = f(a=1) %}` is a keyword argument.

Seen red: accepting unknown tags fails two tests, and dropping the unclosed-block check at
end of template fails two more.

## Progress: the demo fixture is in

`demo/templates_chain.yml` plus `demo/templates/` — the demo tree had **no `.j2` files at all**,
so the differential's demo control could not come back non-zero. It can now, and
`demo_templates_name_exactly_what_jinja2_names` is it: the exact reference set of every demo
template, name for name, against `find_referenced_templates` on 3.1.6 run over those same files.
That test is the rule-4 pin for the fixture's labels as well as the control.

The fixture is a working playbook, not a pile of strings: `ansible-playbook templates_chain.yml`
runs `ok=4 failed=0` on ansible-core 2.21.2, so nothing in it is a shape Ansible would reject.

`templates/broken.conf.j2` sits in `templates/` unrendered by any task, and the play still runs
`ok` — this ticket's central claim, re-measured inside our own demo rather than in a scratch
tree. It is marked **NOT YET FLAGGED**, since the parse is in and the diagnostic is not.

### One template, three answers — measured, and it settles box 3

`templates/common.j2` is a single file containing `{% include "shared.j2" %}`, rendered from
three places. On 2.21.2:

| rendered from | `shared.j2` resolves to |
| --- | --- |
| `roles/edge-proxy` | `roles/edge-proxy/templates/shared.j2` |
| `roles/edge-cache` | `roles/edge-cache/templates/shared.j2` |
| a play-level task | `templates/shared.j2` |

The Traps section said the search path is call-site-dependent; this is that, run. Note what it
rules out: the resolution does not follow the file the include is *written in*, so there is no
"resolve relative to the template's own directory" shortcut that gets this right. Go-to-definition
on that line has three answers, which is why box 3 is the candidates UX and not a lookup.

Seen red: making the dynamic `{% include tuning_file %}` literal, repairing `broken.conf.j2`'s
tag, and hiding the `templates/` directories each fail the pin at a different assertion.

## Progress: the diagnostic ships

`template-syntax`, one ERROR on a `.j2` that will not render. `jinja::will_not_render` is the
core half; `State::template_diagnostics_for` is the message; `publish_diagnostics` takes the
template branch **before** the YAML path.

That ordering is the part with a false positive behind it, and the control shows it: with the
branch disabled, `templates/good.j2` — a template that renders fine — comes back `unparseable`.
True about the bytes, a lie about the file. A `.j2` is not broken YAML, it is a different
grammar, so it never reaches the YAML reader at all.

Tested through `did_open`, not only as a pure function: "the function returns a diagnostic" and
"opening the file produces one" are different claims and only the second is what a user gets.
The nine measured rows each produce exactly one ERROR **and their repaired forms produce none**,
which is the control that stops a reader that refuses everything from passing.

### The extensions gate is wider than the ticket said, and measured

The ticket asked for the *unknown-tag* diagnostic to go quiet when extensions are configured.
It has to be the whole file. `jinja2.ext.Extension.preprocess` rewrites the source **before**
lexing and may return anything, so with an extension loaded no refusal of ours is safe to
report — `{{ x }` included, which has nothing to do with tags.

Measured on ansible-core 2.21.2, both spellings, one template each way:

| | `{% for i in [1,2,3] %}{% if i == 2 %}{% break %}{% endif %}{{ i }}{% endfor %}` |
| --- | --- |
| default | `Encountered unknown tag 'break'`, fatal at render |
| `ANSIBLE_JINJA2_EXTENSIONS=jinja2.ext.loopcontrols` | renders `1` |
| `[defaults] jinja2_extensions = jinja2.ext.loopcontrols` | renders `1` |

`AnsibleConfig::jinja2_extensions` reads both, env beating the file. Default is `[]`, so the
default case is the one that speaks; the key is deprecated as of 2.23.

### The client had to be told templates exist

`.j2` reached no document selector — the client registered `ansible` and `yaml` only, so the
server never saw a template. Now `{ scheme: "file", pattern: "**/*.{j2,jinja,jinja2}" }`, by
glob rather than by language id: VS Code ships no id for `.j2`, so which one a template arrives
under depends on whichever other extension claimed it. **Not run in the editor** — the routing
is pinned by the `did_open` test, the selector change is not.

## Progress: `trim_blocks` / `lstrip_blocks`, measured — and deliberately not modelled

The unmeasured item. Ansible's `template:` module defaults `trim_blocks: True`; stock jinja2
defaults it `False`. Measured on 2.21.2 byte-for-byte with `od`, and **both surfaces agree** —
a `set_fact` on the same string renders identically, so it is the templar's default and not a
module quirk:

| source `A
{% if true %}
B
{% endif %}
C
` | rendered |
| --- | --- |
| module default | `A
B
C
` |
| `trim_blocks: false` | `A

B

C
` |
| `lstrip_blocks` default (false) | leading spaces before a tag survive |
| `lstrip_blocks: true` | they do not |

They are not modelled, and that is a decision rather than an omission. Both drop bytes from the
*rendered output*; modelling them would drop those bytes from our **spans**, which index the
file the user is editing, and a span that does not cover the source is a squiggle in the wrong
place.

That is only safe because neither changes anything this crate reports. Probed against jinja2
3.1.6 over **20,010 templates** (20,000 generated from statement/data/whitespace-control
combinations, plus every `demo/*.j2`) under all four `(trim, lstrip)` combinations: the parse
verdict and the reference set **never differed once**, while the block split differed on 13,286
— so the probe was thoroughly able to see a difference and there was none where it matters.

`nothing_goes_missing_from_a_template_but_raw_tags_and_marked_whitespace` keeps it that way.
Note what that invariant is *not*: "every byte is in a block" is already false, because a `-`
marker shrinks the neighbouring `Data` span and this port follows jinja2 in doing so. The line
is between whitespace dropped because the **template says to** and whitespace dropped because
of a **setting somewhere else**. Seen red by simulating `trim_blocks` in
`apply_whitespace_control`: it names the exact byte.

## Progress: the `#jinja2:` header is read

`jinja::header` and `jinja::document`. `references` now goes through `document`, so a header
changes what the whole crate sees.

**This one closes a live false positive**, which is why it came before resolution. With block
delimiters overridden, `{% notatag %}` is ordinary text — measured, the file renders
`yes{% notatag %}`. Read with the default delimiters we called it an unknown tag and put a red
`template-syntax` ERROR on a template ansible renders without complaint. The demo carries that
exact file, and with the header reader disabled it joins the flagged list — that is the control.

Read from `_jinja_bits.py:76` (the marker) and `:162-192` (the reader), then run:

| rule | measured on 2.21.2 |
| --- | --- |
| `startswith`, so the header is the **first byte** of the file | with a blank line above it, ansible renders the header out verbatim and the override never applies |
| the header line is **removed** before lexing | so spans must be measured from the body — `blocks_from` takes an offset instead of the caller slicing the string |
| the header **beats the module parameters** for fields it names | header `[% %]` won over a task passing `variable_start_string: "<<"`; both the module pair and the default pair stayed literal |
| `split(',')`, then `split(':', 1)`, key `strip()`ed, value through `ast.literal_eval` | both spacing spellings render |

Five refusals, each measured verbatim as `Task failed: Syntax error in template: <this>` and
each reproduced word for word, down to Python's `repr` quoting:

- `Invalid '#jinja2:' override key 'nosuchkey'.`
- ``Missing key-value separator `:` in '#jinja2:' override pair ' variable_start_string"[%"'.``
- `Empty '#jinja2:' override pair not allowed.`
- `Block, variable and comment start strings must be different.`
- `Missing newline after '#jinja2:' override.`

The repaired form of each is accepted, so a reader that refused every header would not pass.

### The oracle has a limit, and the demo now shows it

`find_referenced_templates` knows nothing about `#jinja2:` — it is ansible's, not jinja's. On
`demo/templates/overridden.conf.j2` upstream with default delimiters says
`Encountered unknown tag 'notatag'`; given the header's delimiters and the header line removed
it agrees with us on `partials/header.j2`. So the differential oracle holds for header-less
templates only, and the demo pin says so at the row rather than in a footnote.

### One more thing the render turned up

The overridden delimiters belong to the **whole render**, not to the file carrying the header.
`partials/header.j2` is written with the default `{{ }}` and is pulled into `overridden.conf.j2`;
its `{{ inventory_hostname }}` comes out **literal** while `[% inventory_hostname %]` in the
includer renders. One include chain, two delimiter sets, and only the includer's win. Anything
that later resolves and re-reads an included template has to carry the includer's delimiters
into it rather than reading it fresh.

## Progress: include targets resolve, and go-to-definition works inside a `.j2`

`resolve::template_search_path` and `resolve::resolve_template_include`, with
`Backend::template_definition_at` behind `goto_definition` — the same fork
`publish_diagnostics` takes, because a `.j2` has no YAML key to hang a reference on.

### The search path, measured rather than assumed

`plugins/action/template.py:104-113` builds it as

```text
searchpath = ansible_search_path + [loader._basedir, dirname(source)]
then each p becomes  p/templates,  p
```

and `ansible_search_path` is the include chain of the **task doing the rendering**. Printed
from four places on 2.21.2:

| rendered from | `ansible_search_path` |
| --- | --- |
| a role's `tasks/main.yml` | `[<role>, <role>/tasks, <playbook_dir>]` |
| a file that role includes | the same |
| a play-level task | `[<playbook_dir>]` |
| a task file included from `sub/` | `[<playbook_dir>/sub, <playbook_dir>]` |

**This corrects something written above.** The Traps section says the search path is
call-site-dependent, and it is — but the Progress note on the demo fixture went further and
said there is no "resolve relative to the template's own directory" answer. There is:
`dirname(source)` is appended for **every** caller, so a hit under the template's own directory
is reachable no matter who renders it. That is the one entry that is not a guess, and it is
what makes a jump sound.

The role and project entries are the location-derived rest — right for a role template
including from its own role, and incomplete for a template rendered from a role it does not
live in, whose `templates/` would come first and shadow ours. So the jump means *"a file this
include reaches"*, not yet *"the file this include reaches"*. Box 3 stays open on purpose.

De-duplicated, and upstream is not: for a role template `dirname(source)` is `<role>/templates`,
which the role entry already contributed. First match wins either way, so dropping the repeat
changes no answer and makes the candidate list readable in a diagnostic.

### Controls

Seen red four ways, each on a different assertion: dropping the role entry, dropping the
`dirname(source)` entry, and disabling the `.j2` fork in `goto_definition`.

The `dirname(source)` control needed the fixture fixed first — it was written under
`<root>/templates`, where the **project-root entry finds the sibling anyway**, so the test
passed with the entry removed. Rule 2: it could not have failed for the reason it claimed. The
fixture now lives at `deploy/tpl/`, outside every other root, and the mutation fails it.

## Progress: the call-site link, and the three boxes that were waiting on it

`template: src:` is a reference now (`ReferenceKind::TemplateSrc`), and
`resolve::render_sites` inverts it: given a `.j2`, every `template:` task that renders it,
each with the search path *that task* gives it and any delimiters it passes. Three things that
had no way to be right suddenly do.

**This is a slice of [[T-015]], taken deliberately and kept narrow.** Only `template:` is in
the table. `slurp`, `fetch` and `file` take a path on the managed host, where checking for a
local file warns about correct code, and T-015 owns that table. The reference is also **not
diagnosed** — `src:` navigates and never warns, because the missing-file verdict on `src:` is
T-015's and belongs behind its corpus gate over 385 real `src:` values.

### Candidates, not a guess

`demo/templates/common.j2` holds one `{% include "shared.j2" %}` and is rendered from three
places. Go-to-definition returns **three** locations and the editor shows a picker. Ordered by
task file so the list is the same on every machine — a directory walk's order is not, and with
several call sites there is no "right" first answer to prefer.

### Delimiters from the rendering task

The `template:` module's six delimiter parameters, read off the task and applied to the file.
A `#jinja2:` header still beats them (measured). Sites that **disagree** fall back to the
defaults rather than picking a winner: reading a template with the wrong delimiters is how a
working file gets a red squiggle, and that hazard is the whole reason this half exists.

`demo/templates/module_delims.j2` is the fixture and its own control — read with the default
delimiters it is a false positive (`unknown tag 'notatag'`), and the demo test asserts *both*
that it is clean through the link and that it would be flagged without it.

### The missing-include warning, finally soundable

Held back one round on purpose, and this is what unblocked it: the verdict is "no candidate on
the search path of **any** task that renders this file", which cannot be asked from the
template alone. Conservative in three places, each otherwise a false positive — a template with
no call site we can find is left alone, a dynamic target names nothing, and `ignore missing` is
legal by design. `ignore_missing` is now recorded on `jinja::Reference` rather than merely
tolerated by the parser.

Measured on 2.21.2, and it confirms the search-path construction exactly, duplication included:

```text
Error rendering template: 'partials/nowhere.j2' not found in search paths:
'<demo>/templates', '<demo>', '<demo>/templates', '<demo>', '<demo>/templates/templates',
'<demo>/templates'
```

That is `[playbook_dir, basedir, dirname(source)]`, each doubled — the repeats are ansible's.

### Two things the work turned up

**A path-form bug that read as a feature limit.** The first candidates run came back with one
answer instead of three, and it looked like the documented limitation rather than a defect: the
editor hands over `C:\x\y.j2` while the workspace walk yields `\?\C:\x\y.j2` for the same
file, so the call-site match found nothing and the location-derived fallback quietly answered
alone. Comparison goes through `Fs::canonical` now. A wrong answer that looks like a known
limit is the worst shape this can take, and only a test naming all three files caught it.

**The delimiter rule moved onto the data.** Two readers need it — the diagnostic and the demo
pin — and rule 3 says that belongs in one place, so `delimiters_for_sites` lives beside
`render_sites` rather than in the language server.

## Progress: the render-site walk is cached, because it was a per-keystroke workspace scan

`render_sites` reads and parses the whole workspace, and the server asks for it on every
diagnostic publish and every jump inside a `.j2` — so the previous commit shipped a full scan
per keystroke. Measured on a generated tree at the scale the tickets cite:

| files | per call |
| --- | --- |
| 731 | 42 ms |
| 3000 | 156 ms |

Linear, and not in the place a first guess would put it. At 3000 files the **directory walk is
6 ms** and **reading the files is 75 ms** — so there is no version of this that is cheap to
redo, and skipping work per call cannot fix it. A substring guard (`src` must appear before a
file is parsed at all) took ~20% and is worth keeping, but the answer is a cache.

`State::render_sites`, cleared wholesale whenever a YAML file changes. Coarse deliberately:
any task file may gain or lose a `template:` task, and a map from templates to call sites
cannot say which templates that touches without recomputing the thing being invalidated.
It is the *right* coarseness because **editing a `.j2` never touches YAML**, so the case that
hurts stays warm and the case that clears it pays once.

### The cache test could not fail, and the size assertion is why

"A `.j2` edit must not clear the cache" was asserted as `cache.len() == 1` after the edit.
It passes either way: `did_change` ends in `publish_diagnostics`, which for a `.j2` asks for the
render sites again and refills the entry it just dropped. Removing the guard under test left it
green. Rule 2 — the probe could not produce the other answer.

It asserts `Arc::ptr_eq` now: same allocation, or the entry was dropped and rebuilt. That
version fails the moment the guard goes, naming the per-keystroke case in its message.

### Still to do here

- **`# noqa` for `template-syntax`** — suppression is YAML-comment shaped
  (`Document::is_suppressed`), and a `.j2` has no YAML comments. A `{# noqa: template-syntax #}`
  spelling belongs with T-146's centralisation rather than beside it.
- **the render-site cache is cleared by any YAML edit**, so a repo where task files are edited
  constantly pays the full walk often. A precise invalidation needs the inbound edges [[T-020]]
  builds and the watcher [[T-012]] provides; revisit when either lands.
- `line_statement_prefix` / `line_comment_prefix`: the header now *sets* them, the lexer
  still ignores them — the sixth lexer state is unbuilt

## Done when

- [x] literal `{% include/import/from/extends %}` targets resolve inside `.j2` files
- [x] a missing include warns; a templated include name stays silent
- [x] multi-context templates offer all candidate resolutions rather than guessing one
- [x] a `.j2` include-chain fixture is pinned
- [x] all 14 statement forms parse, with `{% raw %}` and `{# #}` handled in the lexer, and the
      `%}`-in-a-string and `%}`-inside-brackets cases asserted
- [x] a `.j2` that will not render is diagnosed, with the nine measured rows above as the test,
      and silent when `DEFAULT_JINJA2_EXTENSIONS` is non-empty
- [x] overridden delimiters are honoured — both the `#jinja2:` header and the module
      parameters
- [x] a differential gate against `jinja2.meta.find_referenced_templates` over the `.j2` files
      of the pinned corpus trees, in the T-184 shape: env-gated, `#[ignore]`d, printing every
      hit, with a `demo/` control that must come back non-zero — `references_corpus_gate` is
      the gate, `demo_templates_name_exactly_what_jinja2_names` is the control

Docs: https://jinja.palletsprojects.com/en/latest/templates/#import ·
https://docs.ansible.com/ansible/latest/playbook_guide/playbook_pathing.html
