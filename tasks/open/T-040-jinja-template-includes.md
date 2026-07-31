# T-040 — Jinja `{% include %}` / `{% import %}` / `{% extends %}` in templates

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | L    | —          |

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

## Traps / limits

- **Search path is call-site-dependent:** a `.j2` reachable from two roles has two resolution
  contexts (each role's `templates/`). One template → possibly several valid resolutions →
  may need the "candidates" UX (T-029 style), not a single jump.
- Dynamic include names (`{% include some_var %}`) are unresolvable — stay silent.
- This is a second grammar (Jinja) over a second file type — the biggest cost here.

## Done when

- [ ] literal `{% include/import/from/extends %}` targets resolve inside `.j2` files
- [ ] a missing include warns; a templated include name stays silent
- [ ] multi-context templates offer all candidate resolutions rather than guessing one
- [ ] a `.j2` include-chain fixture is pinned

Docs: https://jinja.palletsprojects.com/en/latest/templates/#import ·
https://docs.ansible.com/ansible/latest/playbook_guide/playbook_pathing.html
