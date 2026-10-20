# T-060 — `suspicious-var`: guarded, undefined, and one edit from a real name

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P1       | M    | T-112 | T-062      |

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

## needs decision: the premise did not hold — reject, or rescope?

Measured 2026-10-20. The rule was built as specified (guarded, reached by no definition, within
two edits of a name that does reach the use, underscore pairs ranked first) and run over the
private corpus and seven public trees:

| Tree                | Near misses | Guarded unreached uses |
| ------------------- | ----------- | ---------------------- |
| private corpus      | 52          | 615                    |
| kubespray           | 3           | 48                     |
| the other six       | 0           | 3,902                  |

Read one by one, **none is a bug.** In the private corpus, 32 of the 33 distinct pairs are the
input-normalisation idiom: the "near" name is *defined from* the guarded one,
`_x: "{{ x | default(...) }}"`, so `x` is a deliberate optional input and its `is defined`
guard is correct. The 33rd is the same shape through a differently named derived variable. The
kubespray three are `node`, an input its own `assert` says "must be set explicitly", against
the plural `nodes`. The table under Problem was read off the first group: those pairs are
not "always false" if the underscored name is computed from the plain one.

So the near-miss filter, which the ticket calls the whole feature, selects the idiom rather
than typos. A narrowed rule would have to exclude (a) a suggestion whose definition reads the
guarded name, and (b) a pure prefix/suffix addition (`node`/`nodes`). After both, every tree
reports **zero**, so nothing is left to show the rule finds anything real. Options: reject;
keep it open as a P3 waiting on a corpus with a real case; or ship the narrowed rule on the
strength of synthetic tests alone, which rule 1 argues against.

The attempt (the shared `unreached_uses` split, rule and corpus gate) was reverted, not
committed; the split itself is sound and is the natural base if this is rescoped.

## Done when

- [ ] the corpus near-miss list is reported as WARNING; plain guarded-undefined is silent
- [ ] `_`-prefix mismatches ranked separately from character typos
- [ ] messages say "did you mean", never "wrong"
- [ ] `# noqa: suspicious-var` works; `scan` prints the list
- [ ] corpus gate: findings stay in the tens — if the count explodes, the filter is wrong
      and the rule doesn't ship
