# T-100 — Unknown key in a roles: entry silently becomes a variable

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P1       | S    | T-099 | —          |

## Problem

```yaml
roles:
  - role: web
    tasks_from: alternate.yml    # does not load alternate.yml
    becom_user: root             # does not become anyone
```

Neither line does what it looks like. Both become **variables** named `tasks_from` and
`becom_user`, scoped to the role at precedence level 20, and nothing is ever reported.

`RoleDefinition._split_role_params` (`playbook/role/definition.py:200-224`):

```python
if key not in base_attribute_names:
    # this key does not match a field attribute, so it must be a role param
    role_params[key] = value
```

This is intentional — `role/__init__.py:552-555` calls them "inline variables in role
invocation" — which is exactly why it can never be diagnosed at runtime. The identical typo
on a **task** is a hard error (`task.py:339-342`, `INVALID_TASK_ATTRIBUTE_FAILED` defaults
True), and on a play or block likewise. Only `roles:` swallows it.

## Approach

The legal set is `RoleDefinition.fattributes` — `Base` + `Conditional` + `Taggable` +
`CollectionSearch` + `Delegatable` + `role`. Warn when a key in a `roles:` entry is outside
that set **and** is a keyword that would be valid on a task or on `include_role`
(`tasks_from`, `vars_from`, `defaults_from`, `handlers_from`, `apply`, `public`, `loop`,
`register`, `notify`, `gather_facts`), or is within edit distance 1 of a legal one.

Deliberately *not* every unknown key: passing real parameters this way is the documented
idiom, so a bare unknown name is a false positive. The signal is a key that names something
Ansible has a keyword for.

## Done when

- [ ] `tasks_from:` on a `roles:` entry warns, and says it defines a variable instead
- [ ] a genuine role parameter does not warn
- [ ] the near-miss list is derived from the keyword tables, not hand-typed
- [ ] `# noqa` suppression works, per T-010
