# T-193 — A looped task's register has no module keys, only results

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P3       | S    | —          |

## Problem

## Approach

## Done when

- [ ]

## Problem

A loop changes the shape of the register completely. Measured on 2.21.2, same module, same play:

| task              | register keys |
| ----------------- | ------------- |
| `command: echo solo` | `changed, cmd, delta, end, failed, msg, rc, start, stderr, stderr_lines, stdout, stdout_lines` |
| the same with `loop: [a, b]` | **`changed, failed, msg, results`** |

The per-item results move under `.results`, so `r.results | map(attribute='stdout') | list`
gives `['a', 'b']` while a bare `{{ r.stdout }}` raises
`'dict object' has no attribute 'stdout'` and kills the play.

Fully static: the task carries `loop:` or `with_*`, the register name is known, and the read is
in the same file. No `RETURN` schema, no condition analysis, no cross-play scope resolution —
which makes it the cheapest rule in the register family and the only one not blocked on T-057.

## The exclusion that makes or breaks it

Inside the **registering task's own** `failed_when:`, `changed_when:`, `until:`, `when:`,
`retries:` and `delay:`, the register name refers to the *per-iteration* result, which does
have `rc`/`stdout`. Verified — this play succeeds with `failed=0`:

```yaml
- command: echo "{{ item }}"
  loop: [a, b]
  register: r
  failed_when: r.rc != 0
  changed_when: "'a' in r.stdout"
```

Miss that exclusion and the rule is pure noise. Measured on the 759-file corpus:

| measurement                                          | count |
| ---------------------------------------------------- | ----- |
| registers on a looped task                           | 146   |
| raw reads of a non-`results` key on one              | 55    |
| after excluding the per-item conditional keywords    | **0** |

All 55 were per-item conditionals on the registering task. Every one a false positive.

## Why P3

The corpus has no real instance. The bug is genuine and fatal when it happens, and the rule is
cheap and provable — but nothing here needs it today. Filed because it costs almost nothing to
implement correctly and the failure is a hard crash, not because there is evidence of demand.

If it is built, the 0 above is the acceptance bar: it must still be 0 on this corpus.

## Done when

- [ ] `{{ r.stdout }}` on a looped register fires; `{{ r.results[0].stdout }}` and
      `r.results | map(attribute='stdout')` are silent
- [ ] the per-item exclusion holds for all six keywords, one assertion each — this is the whole
      rule, and a single combined test would let five of them regress unnoticed
- [ ] `changed`, `failed`, `msg`, `results`, `skipped` never fire
- [ ] `with_items` and friends behave as `loop:` does, asserted on at least one `with_*`
- [ ] corpus gate: still 0 hits
- [ ] `# noqa` works, rule id matched exactly
