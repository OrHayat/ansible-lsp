# T-005 — FQCN module and action-plugin navigation

| Status | Priority | Size | Commit  |
| ------ | -------- | ---- | ------- |
| done   | P2       | M    | d722abb |

## Problem

3665 module references. `redhat.ansible` resolves module names and nothing else, and even
there it only looks at `plugins/modules/` — so `ansible.builtin.debug` lands on a stub with
no implementation, or nowhere.

## Outcome

A 3-part dotted mapping key in task position resolves to
`<collection>/plugins/modules/<m>.py`, then `plugins/action/<m>.py`. In-repo collections,
installed collections, and `ansible.builtin` all work.

Two things this had to get right:

- **Key position only.** `example.atlassian.net` appears in this repo's YAML as a URL — a
  3-part dotted name shaped exactly like an FQCN. Only a mapping **key** is ever a module.
- **`plugins/action/` is not a fallback, it's where the code often is.** `debug`, `assert`,
  `fail` and `set_fact` are documentation-only modules: `plugins/modules/debug.py` carries
  the docstring, and the code that runs lives in `plugins/action/debug.py`. Modules run on
  the managed host; action plugins run on the controller. Grepping the shipped
  ansible-language-server binary found 8 references to `plugins/modules` and **zero** to
  `plugins/action`, which is why its navigation dead-ends on these.

Modules from collections that aren't installed are `Skipped`, never warned — that's a
missing dependency, not a typo.
