# T-192 — A register from a check_mode task carries fabricated values, not missing keys

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P3       | S    | —          |

## Problem

## Approach

## Done when

- [ ]

## Problem

The third amplifier alongside [[T-190]] (skipped) and [[T-191]] (failed), and the nastiest,
because nothing breaks. Measured on 2.21.2:

```yaml
- ansible.builtin.command: echo REAL_OUTPUT
  check_mode: true
  register: c
```

```
skipped = True
rc      = 0
stdout  = ''
msg     = 'Command would have run if not in check mode'
```

Every key is **present, with a fabricated value**. `rc = 0` reads as success. `stdout` is empty
rather than absent. So the failure mode is not a crash:

- `when: c.rc == 0` — passes, and the play proceeds as though the command ran
- `'Joined' in c.stdout` — quietly false
- `{{ c.stdout }}` — renders empty, no error anywhere

T-190 and T-191 both end in `'dict object' has no attribute ...`, which is loud. This one ends
in a wrong decision made silently, which is the failure this project exists to catch.

`failed` is `False`, so `when: not c.failed` does **not** guard it. Only `skipped` does.

Not every module behaves this way — one that supports check mode returns real data:

| task under `check_mode: true` | register |
| ------------------------------ | -------- |
| `command: echo REAL_OUTPUT`    | `skipped=True, rc=0, stdout=''` — fabricated |
| `stat: {path: /etc/hosts}`     | `changed, failed, stat` — genuine, stat supports check mode |

So the rule cannot key on `check_mode:` alone; it needs to know whether the module supports
check mode, which is `DOCUMENTATION`'s `supports_check_mode` — another T-057 consumer.

## Why P3: the static trigger barely exists

Measured across the 759-file corpus:

| pattern              | hits |
| -------------------- | ---- |
| `check_mode: true`   | **0** |
| `check_mode: false`  | 2    |

Zero. The usual way into check mode is the `--check` CLI flag, which is runtime state and
invisible to us — the same class as `-e`. A task-level `check_mode: true` is the only static
hook and nobody writes it.

The two `check_mode: false` uses are worth reading rather than dismissing: that is the correct
manual fix for this hazard — force a read-only task to really run so its register is meaningful
under `--check`. Practitioners who hit this already solve it by hand.

So this is filed for the record and to keep the amplifier set complete, not because it is
worth building soon. If it is ever built, the honest scope is narrow.

## Done when

- [ ] fires on the measured repro: `check_mode: true` on a module **without**
      `supports_check_mode`, register read for a value-bearing key, unguarded by `.skipped`
- [ ] silent for `stat` under `check_mode: true`, which returns genuine data
- [ ] silent when guarded by `when: not c.skipped`; asserted that `when: not c.failed` is **not**
      accepted as a guard, since `failed` is False on this path
- [ ] does not overlap T-190 or T-191 — one test per direction
- [ ] corpus gate: re-count `check_mode: true` before building. If it is still 0, say so and
      close this as rejected rather than shipping a rule with no subject.
