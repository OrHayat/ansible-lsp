# T-245 — A play hosts: pattern matching no inventory host is governed by host_pattern_mismatch, which we never read

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P3       | S    | T-099 | —          |

## Problem

When a play's `hosts:` pattern matches nothing in the inventory, what happens is configurable.
Measured on ansible-core 2.21.2 against an inventory holding only `web01`, with a play on
`hosts: nosuchhost`:

| `[inventory] host_pattern_mismatch` | outcome |
| --- | --- |
| unset (default `warning`) | `[WARNING]: Could not match supplied host pattern, ignoring: nosuchhost`, exit **0** |
| `error` | `ERROR!: Could not match supplied host pattern`, exit **1** |

So the same playbook against the same inventory either runs to completion or fails outright,
decided by one ini line we do not read.

| setting | env | ini |
| --- | --- | --- |
| `HOST_PATTERN_MISMATCH` | `ANSIBLE_HOST_PATTERN_MISMATCH` | `[inventory] host_pattern_mismatch` |

The third value is `ignore`, which drops the warning as well.

## Approach

This is the severity dial for a diagnostic we do not have yet: "this play targets a host
pattern no inventory source provides". T-062 owned the inventory-reading half and is closed;
nothing open owns the mismatch question.

The dial maps cleanly once the diagnostic exists — `error` to ERROR, `warning` to WARNING,
`ignore` to silent — which is the same cfg-drives-severity shape `error_on_missing_handler`
and `invalid_task_attribute_failed` already have in `config.rs`. Worth reading at the same
time as building the check, not before: a setting that gates a diagnostic we do not emit
changes nothing.

Depends in practice on the inventory being parsed, so it is only answerable where T-062's
machinery already reaches. P3 because the failure is loud either way — the user sees a warning
or a hard error on the very first run, which is the opposite of the silent-wrong-answer class.

Found by the T-144 `base.yml` audit.

## Done when

- [ ] a play `hosts:` pattern matching no known host is diagnosed, once the inventory is known
- [ ] its severity follows `[inventory] host_pattern_mismatch`, with all three values asserted
- [ ] silent when the inventory is unknown — absence proves nothing (the T-007 rule)
