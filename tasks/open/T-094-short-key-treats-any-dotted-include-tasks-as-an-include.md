# T-094 — short_key treats any dotted include_tasks as an include

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | S    | T-090 | —          |

## Symptom

`community.general.include_tasks: x.yml` is treated as an include and diagnosed for a missing
file. It is an ordinary module name, in a collection that may not define it at all.

## Cause

`references.rs:92-94` takes the last dot-separated component of any key and matches on that.
Ansible recognises exactly three spellings — `include_tasks`, `ansible.builtin.include_tasks`,
`ansible.legacy.include_tasks` (`constants.py:34` plus `utils/fqcn.py:20-31`). Anything else
is just a module.

## Fix

Match the full key against the three legal spellings per action instead of the short key. The
same rule covers `import_tasks`, `include_role`, `import_role`, `include_vars` and
`import_playbook`.

## Done when

- [ ] a third-party FQCN ending in a known action name is not treated as that action
- [ ] `ansible.builtin.` and `ansible.legacy.` prefixes still are
- [ ] one test per action kind
