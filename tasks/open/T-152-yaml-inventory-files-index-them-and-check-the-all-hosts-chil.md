# T-152 — YAML inventory files: index them and check the all/hosts/children shape

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P3       | M    | —          |

## Problem

A YAML inventory (`plugins/inventory/yaml.py`) is a fourth structured kind next to
playbooks/tasks/vars: nested groups where each level allows exactly `hosts`, `vars` and
`children`, host entries carry inline host vars, and the file conventionally starts at
`all`. We neither index it (host/group vars defined there are invisible to the variable
machinery, the gap T-051's messages already concede) nor check its shape — a typo'd
`host:` for `hosts:` silently produces a group with no members at runtime.

Two honest hurdles, why this is M not S:

- **File-kind detection**: an inventory file has no reserved name. Ansible knows by being
  *told* (`-i`, `ansible.cfg inventory=`). The config key is the only static signal we
  have; a shape-based guess ("top-level `all:` with `hosts`/`children`") risks
  misclassifying a vars file, and a wrong ERROR there is worse than the gap. Audit what
  the `auto`/`yaml` plugin pair actually requires (extension allow-list included) before
  choosing.
- **Severity**: the yaml plugin *warns* and skips on many malformed shapes rather than
  failing (`yaml.py` parse methods) — read them whole, mirror the real behaviour.

## Approach

Two halves, in order: (1) detection + indexing — resolve the `inventory` config key,
parse the group tree, feed host/group vars into the same index `group_vars/` uses today;
(2) shape checks per the whole-read of `yaml.py`, at the plugin's own severities.

## Done when

- [ ] inventory files named by the `inventory` config key are indexed: their vars join
      definedness/go-to-definition like `group_vars/` entries do
- [ ] unknown keys at group level (`host:` for `hosts:`) flagged at the severity the
      plugin's own handling justifies, with cites
- [ ] no shape-guessing on files not named as inventory — a vars file can never be
      misflagged
- [ ] `# noqa`-suppressible, demo inventory stays clean

## Config settings that change this (T-144 audit, ansible-core 2.21.2)

Precedence is env -> ini -> default; none of these declare a `vars`, `cli` or
`keyword` rung, so those do not apply (the T-098 pattern).

| setting | env | ini | what it changes |
| --- | --- | --- | --- |
| `INVENTORY_ENABLED` | `ANSIBLE_INVENTORY_ENABLED` | `[inventory] enable_plugins` | List of enabled inventory plugins, it also determines the order in which they are used. |
| `INVENTORY_IGNORE_EXTS` | `ANSIBLE_INVENTORY_IGNORE` | `[defaults] inventory_ignore_extensions`, `[inventory] ignore_extensions` | List of extensions to ignore when using a directory as an inventory source. |
| `INVENTORY_IGNORE_PATTERNS` | `ANSIBLE_INVENTORY_IGNORE_REGEX` | `[defaults] inventory_ignore_patterns`, `[inventory] ignore_patterns` | List of patterns to ignore when using a directory as an inventory source. |
| `TRANSFORM_INVALID_GROUP_CHARS` | `ANSIBLE_TRANSFORM_INVALID_GROUP_CHARS` | `[defaults] force_valid_group_names` | Make ansible transform invalid characters in group names supplied by inventory sources. |
| `DEFAULT_INVENTORY_PLUGIN_PATH` | `ANSIBLE_INVENTORY_PLUGINS` | `[defaults] inventory_plugins` | Colon-separated paths in which Ansible will search for Inventory Plugins. |
