# T-103 — A static field carrying a template is used literally

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P1       | S    | T-099 | —          |

## Problem

```yaml
- command: whoami
  register: "{{ result_var }}"     # registers a fact literally named "{{ result_var }}"
```

Four field attributes are `static=True` and are never templated: `vars`
(`playbook/base.py:696`), `collections` (`collectionsearch.py:34`), `listen`
(`handler.py:27`) and `register` (`task.py:89`). `post_validate_attribute`
(`base.py:550-557`) emits:

```
"register" is not templatable, but we found: {{ result_var }}, it will not be templated
and will be used "as is".
```

A warning, and the braces ship into the variable name. `keyword_desc.yml:35` documents only
the `listen` case, so three of the four are folklore.

`register` is the one that bites: the fact really is created under a name containing braces,
so every later reference to it is undefined and the failure surfaces far from the cause.

## Approach

Flag `{{` in the value of those four keys. The set is closed and comes from the same
`FieldAttribute` tables as T-107, so this should read the tables rather than hardcode four
names — a fifth static attribute added upstream should light up for free.

`vars` is special: it is the *keys* that are not templated while values are, so the check
belongs on key names there.

## Done when

- [ ] a template in `register`, `listen` or `collections` warns
- [ ] a templated **key** in a `vars:` mapping warns; a templated value does not
- [ ] the static set is read from the keyword tables, not hardcoded
- [ ] the message says the braces end up in the literal value
