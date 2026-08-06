# T-104 — hostvars cannot see play, role or task vars

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P1       | S    | T-099 | —          |

## Problem

```yaml
vars:
  api_port: 8080
tasks:
  - debug: msg="{{ hostvars['web1'].api_port }}"   # always undefined
```

`HostVars.__getitem__` calls `get_vars(host=host)` with **no play and no task**
(`vars/hostvars.py:44-57`). So `hostvars[x]` sees inventory, group and host vars, facts and
extra vars — and cannot see play `vars:`, `vars_files`, role defaults/vars/params, or
block/task vars, for *any* host including the current one.

This is structural, not timing: no run order makes it work. A `hostvars[...]` reference to a
name whose only definitions are play-scoped is **provably** always undefined, which is a
stronger claim than T-051 can usually make and needs none of its exemptions.

Second, smaller fact from the same file: `'localhost' in hostvars` is True because the
membership test auto-creates the implicit localhost (`:69-71`), while `list(hostvars)` omits
it (`:73-77`). A `for h in hostvars` loop and an `in` check disagree.

## Approach

The variable index already records a source per definition (`vars.rs`, `VarSource`). This is
a filter over it: for a use inside `hostvars[...]`, consider only sources at or below host/
group level. Everything needed is indexed already — no new source, no new walk.

## Done when

- [ ] `hostvars[h].x` where every definition of `x` is play-scoped is diagnosed
- [ ] the message names which source it found and why that source is invisible here
- [ ] the same name defined in `group_vars/` does not warn
- [ ] a templated host key still resolves the *variable* half
