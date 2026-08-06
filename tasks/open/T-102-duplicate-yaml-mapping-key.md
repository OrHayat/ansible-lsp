# T-102 — Duplicate YAML mapping key

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P1       | S    | T-099 | —          |

## Problem

Two `when:` on one task, or two `name:`, or the same variable twice in a `vars:` block. The
last one wins and the earlier is dead. Ansible warns:

```
Found duplicate mapping key 'when'. Using last defined value only.
```

`_internal/_yaml/_constructor.py:63-82`, under `DUPLICATE_YAML_DICT_KEY` — default `warn`,
also `error` or `ignore` (`config/base.yml:1361-1375`). A warning in a play run scrolls past;
in an editor it is a squiggle on the exact line.

Two wrinkles worth pinning:

- Severity should follow the user's `ansible.cfg`, since `error` is a legal setting and some
  repos use it.
- Ansible tries `json.loads` on **every** file before YAML (`parsing/utils/yaml.py:38-49`),
  so a `.yml` file containing JSON is parsed as JSON and duplicate keys there are silently
  last-wins with no warning at all. Our parser should match that asymmetry rather than
  improve on it.

## Approach

libyaml reports every key event with a mark; collecting duplicates per mapping is bookkeeping
in `parse_libyaml.rs`, not a second parse. This is the cheapest rule on the board — the
information is already flowing through the parser and is currently dropped.

## Done when

- [ ] duplicate keys in a mapping produce one diagnostic per later occurrence
- [ ] severity follows `DUPLICATE_YAML_DICT_KEY` from `ansible.cfg`
- [ ] the JSON path does not warn, matching Ansible
- [ ] a fixture covers duplicates in `vars:`, in a task, and at play level
