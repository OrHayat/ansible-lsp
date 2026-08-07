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

Two things already landed, ahead of the diagnostic itself:

**Last-wins is now correct.** `Node::get` took the *first* occurrence while Ansible takes the
last, so on a duplicate key we resolved and explained the value Ansible discarded — a
duplicated `include_tasks:` linked the dead path, a duplicated `when:` was analysed as the
dead condition. `parse.rs:56` and `ast.rs:302` now search from the end
(`duplicate_key_resolves_to_the_last_value_like_ansible`, `duplicate_import_playbook_takes_the_last`).
That matters for the diagnostic too: **both** entries are still in the tree, so the rule
underlines the later one and the value the rest of the tool reads is that same one.

**The fixture exists.** `demo/duplicate_keys.yml` covers all three placements; the JSON half
is `demo/plays/duplicate_keys_json.yml`. Both live-verified against ansible-core 2.21.2 —
four warnings, each anchored at the later occurrence, and silence for the JSON file.

Still to do: `config.rs` does not read the setting. The ini key is **`duplicate_dict_key`**
under `[defaults]` (not `duplicate_yaml_dict_key`, which is only the env var's name), so it
is one field plus one match arm following `network_group_modules` — including reading
`ANSIBLE_DUPLICATE_YAML_DICT_KEY` *outside* the `if let Some(text)` block, since env beats
the ini file whether or not a config was found. Note T-098 caps how right this can be: we
only ever read `project_root/ansible.cfg`, so we may read the setting correctly from the
wrong file.

## Done when

- [ ] duplicate keys in a mapping produce one diagnostic per later occurrence
- [ ] severity follows `DUPLICATE_YAML_DICT_KEY` from `ansible.cfg`
- [ ] the JSON path does not warn, matching Ansible
- [x] a fixture covers duplicates in `vars:`, in a task, and at play level
