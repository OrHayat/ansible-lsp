# T-060 — `suspicious-var`: guarded, undefined, and one edit from a real name

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P1       | M    | T-062      |

Split out of T-033 (its SILENT class) — the corpus research lives there.

## Problem

A `when:` guarded by `is defined` or `default()` swallows undefinedness: the branch quietly
never runs, forever, and nothing complains. T-051's `var-undefined` deliberately exempts
guarded uses (the author said "may be undefined"), which makes this exact class its blind
spot. The corpus has 61 such variables over 259 uses — and the payload is the near-misses,
an undefined name one edit from a defined one:

| In `when:`          | Actually defined    | Uses |
| ------------------- | ------------------- | ---- |
| `skip_build`        | `_skip_build`       | 8    |
| `snap_uuid`         | `_snap_uuid`        | 2    |
| `snap_ts`           | `_snap_ts`          | 2    |
| `podman_push_image` | `podman_pull_image` | 1    |

`snap_uuid is defined` where only `_snap_uuid` exists is **always false** — that branch has
never run since the day it was written.

## Approach

WARNING, only when all three hold: guarded, defined nowhere (index incl. T-062's inventory
sources), and within edit distance 1–2 of a name that *is* defined. Plain
guarded-undefined with no near-miss is the legitimate optional-flag pattern (259 uses) —
reporting it is a false-positive machine; **the near-miss filter is the feature**.

- Distance scoring: weight `_`-prefix and suffix differences separately from character
  substitutions — `use_pacemaker` vs `ha_use_pacemaker` may be a deliberate second flag.
- Message says **"did you mean `_snap_uuid`?"**, never "this is wrong".
- Extraction reuses `condition::variables()` — its literals/filters/tests/magic handling
  is pinned by `ignores_literals_filters_tests_and_magic_vars`; extend that test first.

## Done when

- [ ] the corpus near-miss list is reported as WARNING; plain guarded-undefined is silent
- [ ] `_`-prefix mismatches ranked separately from character typos
- [ ] messages say "did you mean", never "wrong"
- [ ] `# noqa: suspicious-var` works; `scan` prints the list
- [ ] corpus gate: findings stay in the tens — if the count explodes, the filter is wrong
      and the rule doesn't ship
