# T-109 — Keyword value enums

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | S    | T-106 | T-107      |

## Problem

A handful of keywords take a value from a closed set, and the enforcement is inconsistent
about *when* it fires — which is what makes two of them worth diagnosing.

| Keyword    | Legal values | Enforced |
| ---------- | ------------ | -------- |
| `debugger` | `always on_failed on_unreachable on_skipped never` | load time, fatal (`base.py:206-209`) |
| `order`    | `inventory sorted reverse_sorted reverse_inventory shuffle` | **run** time (`inventory/manager.py:438-439`) |
| `strategy` | any loaded strategy plugin | **run** time (`task_queue_manager.py:380`) |
| `serial`   | int, percent string, or list thereof | `<= 0` silently means "all hosts" (`playbook_executor.py:286-287`) |

`order:` and `strategy:` survive `ansible-playbook --syntax-check` entirely — the run-time
check is reached only when the play actually starts — so a typo there can pass CI and fail in
production. Those two are the reason this is a ticket and not a footnote.

`serial: 0` is not an error at all; it just means everything at once, which is the opposite
of what someone writing `serial:` usually wants.

## Approach

Enum sets are one more column on the T-107 tables. `strategy` needs the plugin index rather
than a literal list, so it should degrade to no-diagnostic when Ansible is not installed, the
same concession the module rules already make.

## Done when

- [ ] `debugger:` and `order:` outside their sets are ERRORs
- [ ] an unknown `strategy:` warns when the plugin index is available, and is silent otherwise
- [ ] `serial: 0` gets a HINT saying it means all hosts
- [ ] the enums live with the keyword tables, not in a second place
