# T-072 — Network modules: one platform action plugin handles the whole family

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| done   | P3       | S    | T-118 | —          |

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

- [x] a fixture collection with `plugins/modules/ios_config.py` + `plugins/action/ios.py`
      hovers the platform plugin, linked, "runs on the controller" — `cisco.ios` in
      `demo/collections/`, pinned by `hover_names_the_network_platform_plugin`, which
      checks `ios_facts` too so "one plugin per family" is asserted rather than implied
- [x] a non-network `foo_bar` module with no `foo` in the list is unaffected
- [x] cfg-overridden `network_group_modules` honoured — ini key, `ANSIBLE_NETWORK_GROUP_MODULES`,
      and the shipped default, in that precedence

## Landed

`network_platform_twin` in `main.rs`, consulted only after the same-name twin search comes
up empty — Ansible's order is twin, then platform, then ship to the host, and the hover now
mirrors it. `module_hover` gains one line naming the platform, because otherwise `ios.py`
under an `ios_config:` task reads as a mismatch rather than the mechanism.

**The decoy is the fixture that matters.** `demo.charlie.link_status` sits beside a
`plugins/action/link.py` — structurally identical to `ios_config` beside `ios.py`. Ansible
ignores it, because `link` is not a platform, and `task_executor.py:961-962` ANDs the list
membership with the plugin's existence. An implementation that only probes for
`<prefix>.py` passes both `ios_*` cases and fails this one, so without it the suite would
go green on the wrong code.

**The cfg override, in full** (`config.rs`): `network_group_modules: Option<Vec<String>>`,
where `None` is "key absent, use the 22 defaults" and `Some(vec![])` is a deliberate "no
platforms" — a list key replaces the default rather than extending it, so an empty `Vec`
could not carry both meanings. It parses as a **comma** list; routing it through
`expand_list` (colon-separated, path-expanding, used by every other key) would have split on
the wrong character and then treated each platform name as a relative path.

`ANSIBLE_NETWORK_GROUP_MODULES` is resolved once in `load_in`, not at each lookup: a
server's environment is fixed at launch, so there is nothing to observe later. It also
forced the missing-`ansible.cfg` early return to become an `if let` — the env layer applies
whether or not a config file exists, and the early return had been skipping it. Its test
lives in its own integration binary (`tests/network_group_modules_env.rs`) because setting a
process-wide variable from one of several threads leaks into whatever else is reading config
at that moment.

**Found on the way, not fixed:** `plugin_twin` locates the twin with
`s.contains("/plugins/modules/")` on a native path, so on Windows it matches nothing and the
same-name twin check silently fails for every collection module. Only the legacy-dir path
(T-073) and the install path are exercised by tests, so nothing caught it. This ticket's
lookup walks path components instead and is unaffected. Filed as **T-086**.
