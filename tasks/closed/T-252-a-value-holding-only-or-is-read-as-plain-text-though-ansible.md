# T-252 — A value holding only {% or {# is read as plain text, though Ansible renders it as a template

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P2       | M    | T-114 | —          |

## Symptom

37 places decided "is this a template?" with `contains("{{")`, a few adding `{%`. Ansible
decides with `is_possibly_template` (`_jinja_bits.py:973`): any of `{{`, `{%`, `{#`, or a
leading `#jinja2:` header. A value holding only `{%` or `{#` was read as plain text.

Measured on 2.21.3 (`scratchpad/t252/shapes`): `include_tasks: "{% if true %}a{% endif %}.yml"`,
the same through `import_tasks`, `a{# c #}.yml`, `include_vars`, `vars_files` and an
`include_role` name all load their file. We warned `missing-file` on all six.

The other direction too: a `{%` or `{#` in a static field or a module argument key is exactly
as literal as `{{` — measured, `register:` and a `vars:` key fail `Invalid variable name`, a
`module_defaults:` key `Could not resolve action`, a `collections` entry draws "not
templatable", a `listen:` topic keeps its braces, and `debug: {"{% … %}msg": x}` fails
`Unsupported parameters`. We said nothing. A `set_fact` key with `{%` *is* rendered, as with
`{{`.

## Cause

No shared rule: each site wrote its own test, most of them `{{` only.

## Fix

`jinja::is_template`, Ansible's check, at every site that asks the question. Not changed, on
purpose: `complex_key.rs`, whose `{{` is YAML reading braces as a flow mapping; the scanners that
take a template apart by hand (`vars.rs:514`, `resolve.rs:500`, `glob.rs`, `bin/scan.rs:189`,
`main.rs:3660`), which are a parser question and not this one; and `vars.rs:492`, a pre-check
for variable uses, which a `{#` comment cannot hold. A parse cannot stand in for the check:
an invalid template is still a template to Ansible.

## Done when

- [x] `jinja::is_template` agrees with `is_possibly_template` on every row of its test, run
  against 2.21.3
- [x] every site asking the question uses it; a test per changed behaviour — templated paths,
  static fields, argument keys — each seen failing with its own site reverted
- [x] corpus scans and editor diagnostics diffed: identical, no corpus value relies on `{%` or
  `{#` alone
