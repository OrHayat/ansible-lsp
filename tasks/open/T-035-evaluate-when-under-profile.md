# T-035 — Evaluate `when:` under a supplied run profile

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P3       | L    | T-121 | T-033      |

## Problem

Today the `when:` hover explains a condition **in the abstract** — "runs unless `skip_demo`
is set", "runs only when `demo_mode = docker`". It never says what would happen on *your*
run, because it has no inputs. The user question it can't answer is the one people actually
ask:

> Given this inventory / these `-e` vars / this `--limit`, which imports and plays will
> actually run — and which will skip?

Ansible only answers that by running (or `--check`-ing) against a live inventory. We can
answer a large slice of it statically, from the same condition analysis T-032 already does,
by evaluating each condition against a **profile** of inputs instead of just classifying its
shape.

Crucially the profile is **partial by design**. Inventory-only should resolve group-
membership conditions and leave `-e`-dependent ones unknown; `-e`-only should do the reverse.
That means three-valued evaluation — **runs / skips / unknown** — never a guess dressed up as
an answer. "Unknown" is a first-class result, same honesty rule as every other feature here:
we do not report "skips" for something we merely failed to evaluate.

## What a profile is

A named set of run inputs, from a committed file (`.ansible-lsp/profiles/*.yml`) or an editor
picker:

- **extra vars** — `-e key=val` and `-e @file.yml`, highest precedence
- **inventory** — groups → hosts, so `groups['x']`, `inventory_hostname`, `'h' in groups.y`
  become decidable. Reuses the ini/yaml inventory parsing T-033 needs for its var index.
- **limit / target host** — which host we're evaluating *as*, so host-scoped conditions have
  a subject
- (later) **tags** — `--tags` / `--skip-tags`, once tag references are modelled

Everything absent from the profile stays unknown. No profile at all → current behaviour
(shape only).

## Evaluation

Extend `condition` from *classify* to *evaluate against an environment*. A three-valued
evaluator over the subset the classifier already understands:

- comparisons `== != < > <= >=`, `in` / `not in`, `is defined` / `is not defined`
- filters that change the value we compare: `default(...)`, `bool`, `int`, `length`
- boolean structure `and` / `or` / `not`, and the list-`when:` implicit AND

Variable lookup follows a **documented, deliberately partial** precedence: profile extra
vars, then the T-033 var index (`defaults/`, `group_vars/`, `host_vars/`, `vars:`). Anything
the evaluator can't reduce — an unsupported filter, a fact, a `set_fact`-defined name, a
name absent from both profile and index — evaluates to **unknown**, and unknown propagates
with three-valued logic:

```
unknown AND false = false      unknown OR true  = true
unknown AND true  = unknown    unknown OR false = unknown
not unknown       = unknown
```

So a two-clause `when:` where one clause is decidably false is **skips**, even if the other
clause is unknown — which is exactly the common "gated on a fact, but also gated on a flag
you didn't set" case.

## Surfaces

- **hover** — add a line under the existing breakdown: *"Under profile `staging`: **skips** —
  `demo_mode` defaults to `native`, needs `docker`."* This is the ✓/✕ default-run verdict the
  hover mockup showed but the shipped hover deliberately left out.
- **`scan --profile staging`** — whole-repo report: for every conditional import, `runs` /
  `skips` / `unknown` under that profile. Turns "what does this deploy actually do on
  staging" into a diffable artifact, and is the real test harness for the evaluator.
- (optional) a decoration dimming plays that skip under the active profile.

## Traps

- **Facts are never static.** `ansible_*`, and anything from `gather_facts`, are unknown
  unless the profile provides them explicitly. Do not treat a missing fact as false.
- **`set_fact` / `register` are runtime.** A name the play assigns later is *unknown*, not
  *undefined* — and if it's the var the condition is gated on, that's the T-032
  `when-import-var-mutated` case, which must still win. Evaluation must not paper over it.
- **Precedence is not fully knowable.** Real Ansible precedence has ~22 levels; we honour a
  named subset and say so. Getting a verdict *wrong* because we mis-ranked sources is worse
  than returning unknown — when two sources disagree and the winner is outside the modelled
  subset, return unknown.
- **The extractor traps from T-033 all apply** — string literals, attribute access, bare
  filter names, Jinja tests, magic vars. Reuse `condition::variables()` and its pinning test;
  do not re-tokenise.
- **Partial must never round up to a full answer.** If any clause needed for the verdict is
  unknown and the structure can't discharge it, the verdict is unknown. A corpus check: on
  `~/app/ansible` with an empty profile, **every** conditional import must report unknown,
  never runs/skips.

## Done when

- [ ] a profile format (extra vars + inventory + limit) loads from `.ansible-lsp/profiles/`
- [ ] `condition::evaluate(cond, &Env) -> Trivalue` over the supported subset, unknown-safe
- [ ] three-valued AND/OR/NOT, verified against the truth table above
- [ ] hover gains a "Under profile X" verdict line when a profile is active
- [ ] `scan --profile <name>` prints runs/skips/unknown per conditional import
- [ ] empty-profile corpus gate: 0 runs, 0 skips, all unknown — no guessing
- [ ] facts and `set_fact`-defined names evaluate to unknown, not false
- [ ] the modelled precedence subset is documented in the message/report, not implied
