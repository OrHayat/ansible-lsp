# T-229 — vars_plugins_enabled without host_group_vars makes every group_vars and host_vars file dead, and we index them anyway

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P2       | S    | T-112 | T-228      |

## Symptom

A cfg line that enables a custom plugin and forgets the built-in one —
`vars_plugins_enabled = my_plugin` — makes every `group_vars/` and `host_vars/` file dead.
Measured 2.21.3 (`scratchpad/t228_review_enabled.sh`, from T-228's independent review): a name
defined only in `group_vars/all.yml` is undefined at task time under that line, and under an
empty `vars_plugins_enabled =`. Ansible says nothing. We index those files regardless, so
`var-undefined` stays silent for every name they define and hover points at a file Ansible never
reads — a project-wide false silence and a false hover from one cfg line.

## Cause

`plugins/vars/host_group_vars.py:70` sets `REQUIRES_ENABLED = True`; the default enabled list is
`host_group_vars` alone, and a user writing the line to enable their own plugin *replaces* the
default rather than extending it. `vars.rs:1803-1854` ports the plugin's file rules
unconditionally, and nothing reads `VARIABLE_PLUGINS_ENABLED` — T-228 is where that reader lands,
which is the dependency.

## Fix

Two halves, both cheap once T-228 reads the list:

- **the index** — when `host_group_vars` is absent from the enabled list, the `group_vars/` and
  `host_vars/` sources are not definitions. Record them as *dead* rather than dropping them, so
  hover can say why a file that looks like a definition is not one.
- **the diagnostic** — a WARNING on the cfg line: "`vars_plugins_enabled` does not list
  `host_group_vars`; every group_vars/ and host_vars/ file is ignored". T-099's shape: provably
  wrong, Ansible silent.

## Done when

- [ ] under a cfg that omits `host_group_vars`, a name defined only in `group_vars/` is reported
      undefined, and hover on the file's definition says it is dead and why
- [ ] the cfg line gets the warning; the default list and an explicit list that includes
      `host_group_vars` get none
- [ ] `# noqa`-suppressible per T-010; the demo cfg stays clean
