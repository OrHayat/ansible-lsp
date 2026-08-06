# T-095 — Templated import_playbook is reported missing

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | S    | T-090 | —          |

## Symptom

`import_playbook: "{{ env }}-setup.yml"` is reported `Missing`. Run with `-e env=prod` it
resolves and runs. This contradicts the README's own rule that templated paths never warn,
which is the one silence the project has always been willing to defend.

## Cause

`resolve.rs:290-297` asserts `Status::Missing` for a templated `import_playbook`, reasoning
that no file is literally named `{{ x }}.yml`. The reasoning is right; the conclusion is not.
`import_playbook` is not a `static=True` attribute, so `PlaybookInclude.load` templates it
before use (`playbook/playbook_include.py:78`) against `variable_manager.get_vars()`.
`import_tasks` is the same (`helpers.py:169`) — and its own error text names which var sources
are legal there: "vars/vars_files or extra-vars ... not facts or inventory".

## Fix

Route templated `import_playbook` through the same glob-the-candidates path as every other
templated reference, and never warn.

## Done when

- [ ] a templated `import_playbook` produces no diagnostic
- [ ] it offers candidates wherever the pattern can be expanded
- [ ] the same holds for templated `import_tasks`
