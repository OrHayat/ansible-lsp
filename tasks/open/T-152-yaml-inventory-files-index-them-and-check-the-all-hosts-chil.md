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
