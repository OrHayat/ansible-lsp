# T-104 — hostvars cannot see play, role or task vars

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P1       | M    | T-099 | —          |

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

## Measured (2.21.2)

Source-derived when filed; run since, against `demo/inventory.ini`. One play, `hosts: all`,
`vars: {play_scoped: 8080}`:

| read                                             | result         |
| ------------------------------------------------ | -------------- |
| `{{ play_scoped }}`                              | `8080`         |
| `{{ hostvars['web01'].app_port }}` (host_vars)   | `8888`         |
| `{{ hostvars['web01'].play_scoped }}`            | **fails**      |
| `{{ hostvars[inventory_hostname].play_scoped }}` | **fails**      |

The last row is the one to lead the message with: the same variable, on the host already
running the task, in the same task — direct read fine, hostvars read fatal. The group_vars
row is the control that proves hostvars was working rather than the fixture being wrong.

Ansible's own error is

```
object of type 'HostVarsVars' has no attribute 'play_scoped'
```

which names an internal type, not the variable, and never says the definition exists and is
out of reach. Our message has to say the part Ansible's does not — which source we found it
in, and why that source cannot be seen from here.

`demo/hostvars.yml` carries all four rows; every label there was checked by running it.

## Approach

The variable index already records a source per definition (`vars.rs`, `VarSource`). The
verdict is a filter over it: for a use inside `hostvars[...]`, consider only sources at or
below host/group level. No new source and no new file reads.

### Correction: there is no use to filter yet

Filed as "no new walk". Measured against our own extractor, that is wrong — the walk
produces **nothing** for these expressions:

| expression                          | extracted today  |
| ----------------------------------- | ---------------- |
| `hostvars['web01'].app_port`        | *(nothing)*      |
| `hostvars['web01']['app_port']`     | *(nothing)*      |
| `hostvars[target_host].app_port`    | `target_host`    |
| `cmd_result.stdout`                 | `cmd_result`     |

`hostvars` is dropped as an injected name and `app_port` is never reached, because the
extractor takes the root of an expression and an attribute/subscript is not one. So the
first job is extracting the *subscripted* name out of a `hostvars[...]` chain, and only
then can any source filter run.

That extraction is also the whole of the GOOD half, which is currently just as broken:
`hostvars['web01'].app_port` resolves to `host_vars/web01.yml` and is perfectly reachable
at runtime (measured, `8888`), and we offer no hover and no jump on it. One change serves
both halves — navigation where the source is visible, a diagnostic where it is not.

Size was filed `S` on the filter-only reading; `M` is the honest number.

## Done when

- [ ] `hostvars[h].x` is extracted as a use of `x` at all — the attribute and the
      `['x']` subscript spelling alike, since neither produces one today
- [ ] `hostvars[h].x` where every definition of `x` is play-scoped is diagnosed
- [ ] the message names which source it found and why that source is invisible here
- [ ] the same name defined in `group_vars/` does not warn, and *does* hover and jump —
      the GOOD half is broken today too, and one extraction fixes both
- [ ] a templated host key still resolves the *variable* half
- [ ] `demo/hostvars.yml`'s four rows are pinned by a test, and its two `NOT YET FLAGGED`
      notes are removed once they are
