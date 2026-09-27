# T-193 — A looped task's register has no module keys, only results

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | task | P3       | S    | —          |

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

- [x] `{{ r.stdout }}` on a looped register fires; `{{ r.results[0].stdout }}` and
      `r.results | map(attribute='stdout')` are silent
- [x] the per-item exclusion holds for all six keywords, one assertion each — this is the whole
      rule, and a single combined test would let five of them regress unnoticed
- [x] `changed`, `failed`, `msg`, `results`, `skipped` never fire
- [x] `with_items` and friends behave as `loop:` does, asserted on at least one `with_*`
- [x] corpus gate: still 0 hits
- [x] `# noqa` works, rule id matched exactly

## Landed

`looped_register.rs`, rule `looped-register-key`, a WARNING on the key. `demo/looped_register.yml`
is pinned exactly with a guard over the rest of the demo; each guard in the rule was broken
once and its test seen red.

Re-measured on 2.21.3, which corrected two things above:

- The aggregate keys are `changed, failed, msg, results, warnings`, plus `skipped` when every
  item skipped. `warnings` is new; it never fires either.
- **Only three of the six keywords see the per-item result.** `failed_when`, `changed_when`
  and `until` on the registering task do (`failed=0`). Its `when`, `retries` and `delay` are
  rendered *before* each item: the first item fails with "'r' is undefined", later ones with
  "has no attribute 'rc'". Those reads are real crashes but not this rule's story, so the
  rule stays silent on anything inside the registering task — one assertion per keyword.

Also silent, each measured working: `is defined` / `is not defined`, `| default` / `| d`,
`r.get(...)`. `r['stdout']` fails like `r.stdout` and fires. A name registered or defined
twice in the file is skipped.

**Known gap, T-122.** The variable walk does not read `failed_when` / `changed_when` / `until`
yet, so a *later* task's `failed_when: r.rc != 0` — measured fatal — is not flagged.
`a_later_tasks_failed_when_fires` holds the correct assertion, ignored against T-122. It also
means the own-task exemption for those three keywords is untested on real input until T-122
lands; re-run the corpus gate then.

**Corpus gate: 0 hits.** The control: with every key allowed through, the rule sees 111 reads
of looped registers there — 104 `.results`, 7 `.changed` — all correct, so the 0 is a result
and not blindness.
