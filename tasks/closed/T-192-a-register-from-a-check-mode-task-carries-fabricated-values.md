# T-192 — A register from a check_mode task carries fabricated values, not missing keys

| Status       | Kind | Priority | Size | Depends on |
| ------------ | ---- | -------- | ---- | ---------- |
| **rejected** | task | P3       | S    | —          |

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

## Rejected — the gate failed, and the discriminator does not exist (2026-10-03)

Two independent reasons, both measured rather than argued.

**1. The corpus gate finds no subject.** The 759-file corpus this ticket counted is not on
this machine, so the count was re-run against the only real tree available — kubespray, 1168
YAML files:

| pattern | hits |
| --- | --- |
| `check_mode: true` | 2 |
| `check_mode: false` | 74 |
| `when:` (control, proves the grep works) | 1960 |

Two is not zero, so the gate as written ("if it is still 0") does not answer itself. Reading
the hits settles it anyway: both are **the same file** in two checkouts of the same repo
(`ks-ext4/` and `kubespray/`), so there is **one** distinct occurrence in the tree, and it is

```yaml
- name: Find docker repo in amzn2-extras.repo file
  lineinfile: { dest: /etc/yum.repos.d/amzn2-extras.repo, line: "[amzn2extra-docker]" }
  check_mode: true
  register: amzn2_extras_docker_repo
```

`lineinfile` under check mode returns **genuine** data (measured below), so this is the
idiomatic probe-without-changing pattern, not the hazard. Subjects for the rule: still zero.

**2. `supports_check_mode` cannot tell the cases apart.** The Approach says the rule "needs to
know whether the module supports check mode, which is `DOCUMENTATION`'s
`supports_check_mode`". Measured on 2.21.2 — all three declare `True`, and they do not behave
alike:

| module | declares `supports_check_mode` | register under `check_mode: true` |
| --- | --- | --- |
| `command` | **True** | `skipped=True, failed=False, rc=0, stdout=''` — fabricated |
| `stat` | True | genuine — `exists=True`, no `skipped` |
| `lineinfile` | True | genuine — `changed=True`, no `skipped` |

`command.py:266` sets `supports_check_mode=True` and then, at lines 307 and 340, handles check
mode by skipping — its own comments call this *"partial check_mode support, since we end up
skipping if we get here"*. The flag means "the framework must not auto-skip me", not "my
register is genuine". Keying the rule on it would fire on `stat` and `lineinfile` as readily
as on `command`: a false positive on the exact pattern the one real-world occurrence uses.

A rule here would need per-module knowledge of whether check mode fabricates, which is not
declared anywhere machine-readable. That is a much larger thing than this ticket's `S`, and it
is unbuildable from the data the ticket assumed.

The Problem section stays as written: the hazard is real, and `rc = 0` on a command that never
ran is exactly the silent-wrong-decision this project exists to catch. What is rejected is the
rule, not the finding. If `--check` ever becomes visible to us (it is runtime state, like `-e`),
revisit this with the amplifier set [[T-190]] and [[T-191]].

## Done when

- [x] corpus gate re-run: 1 distinct `check_mode: true` in the available corpus, on a module
      that returns genuine data — zero subjects, and `supports_check_mode` cannot discriminate
- [ ] fires on the measured repro: `check_mode: true` on a module **without**
      `supports_check_mode`, register read for a value-bearing key, unguarded by `.skipped`
- [ ] silent for `stat` under `check_mode: true`, which returns genuine data
- [ ] silent when guarded by `when: not c.skipped`; asserted that `when: not c.failed` is **not**
      accepted as a guard, since `failed` is False on this path
- [ ] does not overlap T-190 or T-191 — one test per direction
- [ ] corpus gate: re-count `check_mode: true` before building. If it is still 0, say so and
      close this as rejected rather than shipping a rule with no subject.
