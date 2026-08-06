# T-110 — Placement and mutual-exclusion rules

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P1       | S    | T-106 | T-107      |

## Problem

Structural rules that a per-keyword legal set cannot express — each one fatal at load, each
one a closed check against the AST. Nearly all live in `playbook/helpers.py`:

| Rule | Cite |
| ---- | ---- |
| `block:` used as a handler | `helpers.py:104-106` |
| `include_role`/`import_role` inside `handlers:` | `helpers.py:245-247` |
| `loop:`/`with_*` on `import_tasks` | `helpers.py:152-154` |
| `loop:` on `import_role` | `helpers.py:258-260` |
| an imported file that is not a list of tasks | `helpers.py:213-214` |
| `meta: end_role` outside a role, or in a handler | `helpers.py:280-285` |
| `rescue:`/`always:` without `block:` | `block.py:138-142` |
| playbook not a list, empty, or an entry that is not a dict | `playbook/__init__.py:74-91` |
| both `import_playbook:` and `ansible.builtin.import_playbook:` in one entry | `playbook_include.py:41-48` |
| `loop` and a `with_*` together, or two `with_*` | `task.py:252-260` |
| `action:` and `local_action:` together | `mod_args.py:322` |
| two resolvable module keys in one task | `mod_args.py:353-354` |

Two adjacent facts worth encoding with them, both **warnings** rather than errors: an
imported file that is empty warns and continues (`helpers.py:210-212`), while an empty role
`tasks/main.yml` is fully silent; and `local_action:` silently overwrites an explicit
`delegate_to:` (`mod_args.py:303,324`).

`import_playbook` inside a `tasks:` list is documented as broken in its own EXAMPLES —
"This DOES NOT WORK ... because I'm inside a play already" (`modules/import_playbook.py:57-64`)
— which makes it a placement rule too.

## Approach

One pass over the semantic AST from T-044. No resolution, no index; each rule is a shape test
on a node and its parent.

## Done when

- [ ] every rule in the table above is an ERROR with the message Ansible would give
- [ ] the two warning cases are warnings, not errors
- [ ] `local_action:` overwriting `delegate_to:` warns
- [ ] a fixture file exercises each rule, good and bad
