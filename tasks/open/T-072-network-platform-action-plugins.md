# T-072 — Network modules: one platform action plugin handles the whole family

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P3       | S    | —          |

Escape #2 from the T-029 module-hover discussion: the same-name twin check false-negatives
on every network module.

## Problem

Network device modules don't get per-module action plugins — one plugin per **platform**,
named after the prefix before the first `_`, handles the whole family: `cisco.ios.ios` runs
every `ios_*` task on the controller (it owns the persistent device connection).
`task_executor.py:939-947,961-962`: prefix = `module_name.split('_')[0]`; if it's in
`NETWORK_GROUP_MODULES`, the handler is `<collection>.<prefix>`.

The hover's twin check looks for `plugins/action/ios_config.py`, finds nothing, and labels
`cisco.ios.ios_config` "module" — wrong on both facts: it runs on the controller, via a
plugin the hover never found.

The platform list is config, not constant: default
`[eos, nxos, ios, iosxr, junos, enos, ce, vyos, sros, dellos9, dellos10, dellos6, asa,
aruba, aireos, bigip, ironware, onyx, netconf, exos, voss, slxos]`
(`config/base.yml:1779-1788`), overridable via `ansible.cfg` `network_group_modules` /
`ANSIBLE_NETWORK_GROUP_MODULES`.

## Approach

In `module_hover`, when no same-name twin exists: split the prefix; if it's in the platform
list, look for `plugins/action/<prefix>.py` in the winner's tree and label it — "handled by
platform action plugin `ios` (runs on the controller)", linked. Ship the default list;
reading the cfg override can ride on `config.rs` (it already parses `ansible.cfg`) or be
recorded as deliberately skipped.

## Done when

- [ ] a fixture collection with `plugins/modules/ios_config.py` + `plugins/action/ios.py`
      hovers the platform plugin, linked, "runs on the controller"
- [ ] a non-network `foo_bar` module with no `foo` in the list is unaffected
- [ ] cfg-overridden `network_group_modules` honoured, or its skip recorded here
