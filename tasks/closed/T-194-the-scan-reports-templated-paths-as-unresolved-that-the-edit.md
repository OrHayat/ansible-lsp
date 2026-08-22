# T-194 — The scan reports templated paths as unresolved that the editor navigates

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | task | P2       | S    | —          |

## Problem

The scan and the editor give different answers about the same reference. In `~/app/ansible`:

```yaml
# roles/r/tasks/helper.yml:39
include_tasks: "{{ chosen_task }}"
```

`chosen_task` is a plain literal in that role's own `defaults/main.yml`. Measured
through the public API on a fixture with the same shape:

```
resolve_in    -> Skipped,  targets=[]                         <- what bin/scan calls
resolve_with  -> Resolved, targets=[.../chosen.yml]   <- what the LSP calls
```

So the editor navigates it and the scan prints it under `TEMPLATED, MATCHES NOTHING`. Two
consumers, one question, two answers — the situation CLAUDE.md rule 3 exists to prevent.

`bin/scan.rs:102` calls `resolve_in`, which takes no literals. `main.rs:559` calls
`resolve_with_in` with `vars::known_literals_in`. Nothing is wrong with either; the scan simply
never asks the better question.

## Approach

Point the scan at the substituting entry point and give it the literals map it already has the
inputs for — it walks vars for `undefined_uses_in` through the same `ScanCache`.

**This cannot change the exit code.** `resolve_with` stamps `SkipReason::Templated` and turns
Missing into Skipped, so a substituted path is navigable but never warned about. The only thing
that changes is the honesty of the report.

**Measure the cost before believing it is free.** The `unknown-host` rule went from 140ms to
4.42s on exactly this kind of assumption (T-179), and was only saved by moving a cheap check
first. The var walk is cached, so this is probably small — but "probably" is what that episode
was made of.

Expected effect is modest and should be stated rather than oversold: of the 9 `TEMPLATED,
MATCHES NOTHING` lines on the corpus, this moves roughly one. Four are `{{ role_path }}`, whose
expansion is deliberately disabled pending T-068, and one is
a two-token path whose second token subscripts a map, which is not a bare identifier and
cannot substitute until [[T-188]] lands.

T-135 covers collapsing the four resolve entry points into one. That is a refactor; this is the
behaviour change, and it does not need the refactor first.

## Done when

- [x] the scan resolves a templated path whose variable has a known literal, asserted by a
      `scan_cli` test with the two-file shape above
- [x] the exit code is unchanged for that fixture — a substituted path must never fail the gate
- [x] a templated path with no known literal still lands in `TEMPLATED, MATCHES NOTHING`
- [x] scan runtime on the corpus measured before and after and recorded here — **no
      detectable change**, numbers below
- [x] the corpus `TEMPLATED, MATCHES NOTHING` count recorded before and after — **10 -> 9**,
      and the one that moved is the reference this ticket was filed on

## What landed

`bin/scan.rs` builds the literals map per file and calls `resolve_with_in`, the entry point
`main.rs` already used. No resolution logic changed — one consumer stopped asking a worse
question than the other. The map is built only when the file actually carries a templated
reference, since it walks every variable source reaching the file and most files have none.

Two tests in `tests/scan_cli.rs`, both seen red first:

| test | asserts |
| ---- | ------- |
| `the_scan_substitutes_a_known_literal_into_a_templated_path` | the ticket's two-file shape: `{{ chosen_task }}` leaves `TEMPLATED, MATCHES NOTHING` and counts resolved, while `{{ never_defined_anywhere }}.yml` in the same file stays in it |
| `the_demo_s_knowable_include_vars_path_resolves_for_the_scan_too` | rule 4: `demo/include_vars_demo.yml` labels its `"vars/{{ env }}.yml"` row "navigates to vars/prod.yml", and nothing pinned that claim while the scan answered `Skipped` |

Plus two helpers: `section()` reads the lines under one heading and `kind_row()` reads one row
of the counts table. Substring matching over the whole report cannot tell a path listed under
`TEMPLATED, MATCHES NOTHING` from the same path under `MISSING FILES`, which is the whole
distinction being measured.

The first fixture put the include in a file *beside* `tasks/main.yml`, so the role had no
default entry point, came back `missing`, and the walk found `0 defs` — the test would have
failed for a reason that had nothing to do with the rule. Before fixing it, a throwaway probe
confirmed the fix *can* deliver what the test asks (rule 1, and the T-178 lesson about scoping
a box to a consumer that cannot answer it). Identifiers below are the fixture's current
neutral ones; the probe itself ran under the earlier names:

```
defs=1 literals={"<var>": ["<target>.yml"]}
{{ <var> }}                       resolve_in=Skipped/0  resolve_with_in=Resolved/1
{{ never_defined_anywhere }}.yml  resolve_in=Skipped/0  resolve_with_in=Skipped/0
```

### Measured on the corpus (`~/app/ansible`, 768 files)

The whole diff of the report — one row, and the section this ticket is named after:

```
 include_tasks   312  0  10     before
 include_tasks   313  0   9     after
 TEMPLATED, MATCHES NOTHING (10) -> (9)
```

`import_playbook`, `import_tasks`, `include_vars`, `module`, `role`, `tasks_from` and
`vars_files` are byte-identical, and `MISSING FILES` stays at 1 — the gate cannot have moved,
which is the claim the Approach makes and this is the measurement of it.

**Runtime: no detectable change.** Median of 10 warm runs, release build:

| build | median |
| ----- | ------ |
| with fix, first pass | 556ms |
| baseline (`resolve_in`, no literals walk) | 565ms |
| with fix, second pass | 570ms |

The two fixed-build medians *bracket* the baseline and the samples overlap almost entirely
(521–563, 523–581, 518–577), so the difference is below run-to-run variance. Stated that way
rather than as a speed-up: the change cannot make the scan faster, and a single ordering that
says it does is noise, not a result.

The walk barely grew, which is why:

```
 var-walk: 4883 edges -> 590 files, 916 reads, 32088 defs     before
 var-walk: 4912 edges -> 591 files, 917 reads, 37421 defs     after
```

**+29 edges, +0.6%**, and one extra file read in total. T-179's failure mode does not repeat
here. (On `demo/` the same measurement gave +458 edges, +54%, and ~+5ms — see below. The
mechanism behind that gap is *not* settled: the first hypothesis, that unmemoized truncated
walks in `contribution_in` were the cost, was tested against a cyclic vs acyclic fixture and
came back +1 edge for both. The remaining candidate is `undefined_uses_in`'s early return for
non-playbook files, since `include_tasks` lives in task files — unverified, and it is a
question about `demo/`, not about whether this change is safe on a real tree.)

### The 9 that remain, and why none of them is this fix's gap

Every one has a ticket:

| count | cause | ticket |
| ----- | ----- | ------ |
| 6 | `{{ role_path }}` — expansion deliberately off, since it comes from the *invoking* role | T-068 |
| 1 | two tokens, one of them a subscript into a map — not a bare identifier, so `substitute_literals` refuses it | [[T-188]] |
| 2 | the literal exists, but the **caller** passes it in | [[T-020]] |

That last row is worth recording, because the obvious reading of "no literal found" is "the
value is a runtime fact" and **that is wrong here**. Both were checked by hand: each is a
plain string literal set in the `vars:` of the `include_role` that invokes the role, and in
both cases the file the reference would name is present on disk. Neither is unknowable — they
sit one edge away in the direction `vars::definitions` documents itself as not following:

> Not exhaustive: variables injected by a *caller* (a playbook that includes this file and
> passes vars), plus inventory and `-e`, aren't visible here

One of the two has its answer written out by hand in a code comment two lines above the
reference, naming the exact pair of files it can expand to — a fair measure of how knowable
it is, and of what the tool is failing to say.

So T-020 gains two concrete, verified cases, and the honest count for this rule is that **3 of
the 10 were substitutable, and 1 of the 3 needed only the file's own variable walk** — which
is the one this ticket fixes.

### Also measured on `demo/` (110 files)

The entire diff of the report:

```
 var-walk: 841 edges -> 72 files, 137 reads, 527 defs      before
 var-walk: 1299 edges -> 75 files, 137 reads, 668 defs     after
 include_vars   3  1  2                                    before
 include_vars   4  1  1                                    after
```

Runtime, median of 10 warm runs, release build. The third row is what shipped; the middle row
exists to attribute the cost, and does — `resolve_with_in` itself is free and the walk is all
of it:

| build | median |
| ----- | ------ |
| `resolve_in`, no literals walk | 56ms |
| `resolve_in`, literals walk computed | 61ms |
| `resolve_with_in` + walk (shipped) | 61ms |

**+5ms, ~9%** — but the samples ranged 54–59 and 58–62, so on a tree this small that is barely
outside the noise and must not be carried over to a real repo. Reads did not move (137 both
ways; the cache had those files), so the cost is **graph edges**, up 54%. Edges grow with
include/role depth, not file count — which is why this number was never allowed to stand in
for the corpus measurement above, and the corpus turned out to behave nothing like it.

**`TEMPLATED, MATCHES NOTHING` is 6 before and 6 after.** Not a null result being explained
away: that section is filtered to `IncludeTasks | ImportTasks`, and demo's six are three
`{{ role_path }}` (expansion deliberately off pending T-068) plus `ap_protocol`, `anything`,
and a `protocol` glob that matches nothing. None is literal-backed, so on this tree the
section *cannot* move. The one reference that moved is an `include_vars`, visible only in the
counts table. The ticket predicted "roughly one line" and one line moved — in a different
section than predicted.
