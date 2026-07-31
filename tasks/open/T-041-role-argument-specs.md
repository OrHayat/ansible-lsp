# T-041 — `meta/argument_specs.yml` role signatures

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | M    | —          |

## Problem

A role's `meta/argument_specs.yml` is the **only** static "function signature" Ansible has:

```yaml
argument_specs:
  main:
    options:
      db_host: { type: str, required: true }
      db_port: { type: int, default: 5432 }
```

At a call site — `include_role` / `import_role` / `roles:` with `vars:` — nothing checks the
vars passed against that spec. Missing a required arg, or passing an unknown one, is a runtime
failure (or a silent no-op) today.

## Approach

When a role has `meta/argument_specs.yml`, parse it and, at each call site that passes `vars:`:

- **completion** for the role's declared option names
- **diagnostic**: required option not supplied → warning; unknown option supplied → hint
- **hover**: the option's type/default/description

Keys under the matching entry point (`main`, or the `tasks_from:` name).

## Traps / limits

- Most roles **don't** ship a spec — absence means no diagnostics, never "role takes no args".
- Roles freely read undeclared vars (from inventory, group_vars, `-e`), so "unknown option"
  must be a soft **hint**, not an error — the spec can lag the role.
- Vars can arrive from many precedence levels, not just the call site's `vars:`; only flag a
  *required* arg missing when nothing in obvious scope provides it (leans on T-033's var index).

## Done when

- [ ] a role with `argument_specs.yml` gets completion for its options at call sites
- [ ] a missing required option warns; an unknown option is a soft hint
- [ ] type/default/description show on hover
- [ ] roles without a spec produce nothing
- [ ] entry point is keyed off `tasks_from:` when present

Docs: https://docs.ansible.com/ansible/latest/playbook_guide/playbooks_reuse_roles.html#role-argument-validation
