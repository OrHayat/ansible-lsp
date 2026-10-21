# T-253 — Navigation splits templated paths by hand and only sees {{ }}, so a {% %} path has no jump target

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P3       | S    | T-114 | —          |

## Problem

Four places take a templated path apart with their own `find("{{")` loop instead of
`jinja::blocks`: `resolve::substitute_literals` (fills `{{ distro }}` with known values),
`glob::to_pattern` and `glob::implied_values` (turn the value into a glob and back), the hover's
`template_idents` in `main.rs`, and the variable tally in `bin/scan.rs`. A `{% … %}` block or a
`{# … #}` comment stays in the pattern as literal text, so `include_tasks: "{% if p %}a{% endif %}.yml"`
offers no file to jump to, though Ansible loads `a.yml` (measured on 2.21.3, T-252).

Navigation only: none of them can make or silence a warning, since T-252 marks such a value
templated before they run. No value in the 45 corpora has `{%` or `{#` without `{{`.

`vars.rs::template_uses_with` is not one of them: it already uses the parser and keeps a `{{`
scan only as the fallback for a template still being typed.

## Approach

One helper over `jinja::blocks` that splits a value into text, bare `{{ name }}` variables,
and anything else; the four sites read that. A comment renders to nothing; a statement or a
non-bare expression is a `*` in a glob and stops substitution, as a filter does today.

## Done when

- [ ] the four sites read the shared split
- [ ] `{% if p %}a{% endif %}.yml` and `a{# c #}.yml` offer `a.yml` to jump to, pinned by a test
