# T-150 — File-kind coverage matrix: every YAML kind, its schema, our status

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P2       | S    | —          |

## Problem

`Ast` classifies by *content* (`Playbook | Tasks | Other`), but an Ansible project has many
more file kinds than that, most distinguishable only by *path*, each with its own schema
and its own validator inside ansible-core. Coverage decisions have been made one file at a
time (T-147..T-149 came out of one conversation); nothing records the full universe, so
"did we miss a kind?" has no checkable answer. T-107's audit showed what that costs.

The matrix, as verified so far (read/indexed = consumed by us today; validated = keys
checked):

| Kind | Schema owner in core | Read | Validated | Ticket |
| ---------------------------------- | ------------------------------------------------ | ---- | --------- | ------ |
| playbook                           | `FieldAttribute` MRO (`base.py:93-105`)          | yes  | yes       | ~~T-107~~ |
| tasks / handlers files             | same + include restrictions                      | yes  | yes       | ~~T-107~~ |
| role `meta/main.yml`               | `RoleMetadata` (`metadata.py:32-41`)             | deps only | no   | T-147  |
| role `meta/argument_specs.yml`     | `arg_spec.py` validator                          | no   | no        | T-041 (call sites), T-149 (the file) |
| collection `meta/runtime.yml`      | collection loader (unaudited)                    | routing only | no | T-148 |
| collection `galaxy.yml`            | `galaxy/data/collections_galaxy_meta.yml` — machine-readable, per-key `required`/`type` | no | no | T-151 |
| `requirements.yml`                 | `cli/galaxy.py:778-834` (list or roles/collections dict) | no | no  | T-151  |
| YAML inventory                     | `plugins/inventory/yaml.py` (`all`/`hosts`/`vars`/`children`) | no | no | T-152 |
| INI inventory, bare group_vars     | `plugins/inventory/ini.py`                       | no   | n/a       | T-062  |
| `group_vars/` `host_vars/` (.yml)  | vars files — keys are user-chosen names          | yes (indexed) | n/a | — |
| `defaults/` `vars/` files          | same                                             | yes (indexed) | n/a | — |
| playbook `.meta` files             | `play.py:452-475` — playbook-level `argument_specs`, found in the T-107 full read | no | no | T-153 |
| `ansible.cfg`                      | `config/base.yml`                                | yes  | partly (per-setting) | ~~T-098~~, T-144 |
| vault-encrypted files              | `$ANSIBLE_VAULT` header                          | no — parses as one scalar, `Ast::Other`; but a vaulted vars source makes `var-undefined` fire falsely (probed) | n/a | T-154 (bug) |
| `files/`, `templates/` (Jinja)     | not YAML                                         | n/a  | n/a       | out of scope here |

Non-core files (`.ansible-lint`, `ansible-navigator.yml`, `execution-environment.yml`)
are deliberately absent: we mirror ansible-core, not its ecosystem.

## Approach

This ticket is the register, not the work: verify each unverified row the T-107 way (read
the consuming source path whole, record cites), correct the table where wrong, and keep it
current as kinds get tickets. A row may resolve to "won't do" — that's a recorded outcome,
not a gap.

## Done when

- [ ] every row's Read/Validated status is source-verified, with cites in this file
- [ ] every row has a ticket, a struck ticket, or a recorded won't-do rationale
- [ ] a "how to add a row" note: any newly-discovered kind lands here first
