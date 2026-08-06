# T-105 — delegate_to: empty template, and hosts not in inventory

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | S    | T-099 | T-062      |

## Problem

Two faults on one keyword.

**A host that is not in inventory is silently fabricated.** `VariableManager` constructs one
on the spot — `delegated_host = Host(name=delegated_host_name)` (`vars/manager.py:546-547`).
A typo in `delegate_to:` produces a real connection attempt to a host nobody defined, and
Ansible never says the name was unknown.

**A template that renders empty is a run-time error**: `Empty hostname produced from
delegate_to: "%s"` (`manager.py:535-536`).

The first is statically checkable against the inventory index, with a hard limit: dynamic
inventory means absence is not proof. So the rule has to be "warn only when the workspace has
file-based inventory and the name is neither in it nor templated" — the same concession T-062
and T-065 already make.

## Approach

Needs T-062's inventory index. Until it lands there is no host set to check against.

Cheap half available now: `delegate_to` whose value is a template that can only ever produce
an empty string — a bare `{{ var }}` where `var`'s only definitions are empty — but that is
rare enough not to justify shipping alone.

## Done when

- [ ] `delegate_to:` naming a host absent from file-based inventory warns
- [ ] no warning when any inventory source is dynamic, or the value is templated
- [ ] the message says Ansible will fabricate the host rather than fail
- [ ] `delegate_to: localhost` and `127.0.0.1` never warn
