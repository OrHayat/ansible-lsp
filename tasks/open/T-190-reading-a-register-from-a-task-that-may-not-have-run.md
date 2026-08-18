# T-190 — Reading a register from a task that may not have run

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P3       | M    | —          |

## Problem

## Approach

## Done when

- [ ]

## Problem

```yaml
- command: echo hello
  when: feature_enabled          # false today
  register: r

- debug:
    msg: "{{ r.stdout }}"        # fatal
```

Measured on 2.21.2 — a skipped task still sets the register, but only to the skip result:

```
r.keys() == [changed, failed, false_condition, skip_reason, skipped]
fatal: object of type 'dict' has no attribute 'stdout'
```

Every module-specific key is gone, including any documented `returned: always`. So no `RETURN`
schema is needed to know this: on the skip path there is nothing module-specific to find.

## Why this is P3 and not P1

Because the corpus says the obvious rule is unshippable. Measured over 759 files:

| measurement                                              | count |
| -------------------------------------------------------- | ----- |
| `register:` total                                        | 1504  |
| registered on a task that also has `when:`               | 454   |
| reads of those, unguarded by `default()`/`is defined`     | 514   |
| after excluding reads inside a `when:`/`failed_when:`/`until:` | 174 |

514 is not a diagnostic, it is a wall of squiggles. And the 174 is **contaminated**: it lists
`retry_defaults.health_check.retries`, and `retry_defaults` is not a register at all — it is a
plain dict in `group_vars/all.yml:1131`. The scan matched by name across the whole tree, while a
register is scoped to its play and host. So the true count is not 174 and is currently unknown.

Two things must exist before this can even be measured honestly, let alone shipped:

1. **Scope resolution** — resolve `r` to the `register:` in the same play, not to any file in the
   workspace that happens to use the name. Without it the rule reports on plain variables.
2. **Condition implication** — the dominant safe pattern is a later task carrying the *same*
   `when:`, so the read only runs when the register exists. Deciding that requires asking whether
   one condition implies another, which is [[T-035]]/[[T-180]] territory, not string equality.

Until both exist, the only shippable slice is the certain case: the registering task's `when:`
classifies as `Verdict::Never` (literal `when: false`), so the task **always** skips and the read
**always** fails. Rare in real code, but it is a genuine error rather than a maybe.

## Done when

- [ ] the `Verdict::Never` slice fires, with the measured repro above as the fixture
- [ ] silent when the reading expression is guarded by `default()`, `is defined`, `is skipped`,
      or `is not skipped` — one assertion each, since they are separate code paths
- [ ] silent for the five skip keys themselves (`changed`, `failed`, `skipped`, `skip_reason`,
      `false_condition`), which are always present
- [ ] a register is resolved within its play, proven by a fixture where another file uses the
      same name for a plain variable and gets no diagnostic — this is the `retry_defaults` false
      positive, pinned
- [ ] the corpus count is re-measured with scope resolution in place and recorded here. If the
      unguarded count is still in the hundreds, the wider rule is rejected, not shipped quietly.
