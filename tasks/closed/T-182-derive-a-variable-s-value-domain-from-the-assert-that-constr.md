# T-182 — Derive a variable's value domain from the assert that constrains it

> **Rejected as a duplicate of [T-180], which is the better ticket** — it carries corpus
> evidence this one guessed at (107 files with an `assert`, and the complete spdk-bdev case
> where our scan reports `TEMPLATED, MATCHES NOTHING` on a line the author documented as
> provably closed), and it gets the `| default()` direction right. Filed independently and
> minutes apart, from the same T-179 route. The two notes below that T-180 lacked have been
> moved into it; nothing else here is worth keeping.
>
> [T-180]: T-180-derive-a-closed-value-set-from-an-assert-that-dominates-a-us.md

| Status       | Priority | Size | Epic  | Depends on |
| ------------ | -------- | ---- | ----- | ---------- |
| **rejected** | P2       | L    | T-112 | —          |

## Problem

```yaml
- ansible.builtin.assert:
    that: kind in ['web', 'db']

- ansible.builtin.include_tasks: "{{ kind }}.yml"
```

`kind` holds `web` or `db`. The playbook says so, two lines up, and the run cannot proceed
past the assert with anything else. We do not read it, so every rule that meets
`{{ kind }}.yml` treats the value as unknown and stops.

This is type checking, but not the kind [T-106] does. That epic transcribes **ansible's**
closed keyword schema from six `FieldAttribute` tables — a fixed vocabulary with declared
types. This is the domain of a **user's** variable, inferred from their code; there is no
table to transcribe and nothing is closed in advance. Same family, different machinery, which
is why it sits under [T-112] with the other "what do we actually know about this name"
tickets rather than under T-106.

**It is already load-bearing in three places, none of which own it.** Each names reading an
`assert` as the reason it stops short, and each treats it as somebody else's problem:

| ticket  | what it gives up on                                            |
| ------- | -------------------------------------------------------------- |
| [T-071] | globs the disk to guess the set; stays an INFO hint, partly because "the value may be validated … by an `assert`, or by `meta/argument_specs.yml` `choices:`, none of which we read today" |
| [T-179] | must silence `unknown-host` for a file whose reachable set contains `include_tasks: "{{ kind }}.yml"` — the file set is unknowable, so the rule stops answering |
| [T-041] | the same job from a different source: `choices:` in a role's argument spec |

Three notes saying "we do not read asserts" and no work item. That is the gap.

## Approach

A variable gets a **closed domain** — a finite set of plain values — or it gets nothing.
Partial knowledge is worse than none here: every consumer below uses the domain to *stop*
conceding, so a domain that is narrower than the truth converts a quiet rule into a
confidently wrong one.

Two halves, and the second is the hard one.

**Reading the constraint.** `that:` is a Jinja expression or a list of them, ANDed. The forms
worth recognising, and no others until measured:

- `kind in ['web', 'db']` — the direct case
- `kind == 'web'` — a domain of one
- `kind == 'web' or kind == 'db'` — the or-chain spelling of the first
- a list of `that:` clauses, which AND, so the domain is the **intersection**

Anything unrecognised yields no domain, which is the safe direction.

**Proving the assert governs the use.** This decides whether the ticket is sound at all:

- same play, and the assert runs **before** the use — an assert after the include constrains
  nothing about it
- the assert is not itself skipped: its own `when:`, or a `when:` on an enclosing block,
  makes the constraint conditional and therefore not a constraint
- the use is not reachable by a path that bypasses the assert — a second include of the same
  file from a play with no assert is the case that breaks a naive implementation
- `ignore_errors` / `failed_when: false` on the assert means the play continues regardless,
  so it constrains nothing

[T-032]'s static `when:` work and [T-020]'s reverse index are what make the last two
answerable; check whether this should depend on them rather than re-deriving reachability.

## Consumers, and what each does with a domain

Per rule 3, list them before deciding where the rule lives — the domain belongs on the
variable, not inside whichever rule asks first.

- [T-071] replaces the disk glob with exact substitution of each allowed value. Its own
  severity ladder then falls out: every value misses → the include is broken for **every**
  permitted input, which is a WARNING rather than a hint.
- [T-179] resolves a templated include edge to a known set of files, so `unknown-host` keeps
  answering instead of silencing the file.
- `var-undefined` and the hover both gain a "this is one of …" line.

## Traps

- **A domain is not a definition.** `assert` constrains a value; it does not define one. A
  variable with a domain and no definition is still undefined, and this must not exempt it.
- **The under-approximation in [T-071] is the opposite shape.** There, the derived set may be
  too *small* (a runtime value with path separators reaches files the glob never saw). Here a
  wrong domain is too small in a way that makes rules *fire*. The two must not be conflated
  into one "candidate values" concept without keeping that direction straight.
- Templated members — `that: kind in [a, b]` where `a` is itself a variable — yield no domain.

## Done when

- [ ] each recognised form yields the measured domain, one test per form, with an
      unrecognised form yielding `None` as the control
- [ ] a list `that:` intersects rather than unions, pinned by a case where the two differ
- [ ] every governance rule above has a test where the assert is present but does **not**
      govern, and the domain is therefore withheld — after, skipped by `when:`, bypassed by a
      second entry point, `ignore_errors`
- [ ] the domain is exposed on the variable, not inside one rule, and at least two consumers
      read it — asserted per consumer
- [ ] [T-071]'s hint upgrades to exact substitution on a closed domain, and its INFO/WARNING
      ladder is asserted at both rungs
- [ ] corpus: every domain derived from `~/app/ansible` is spot-checked against what the
      playbook actually permits; a domain narrower than the truth is a bug, not a finding

[T-020]: T-020-reverse-index.md
[T-032]: T-032-static-when-evaluation.md
[T-041]: T-041-argument-specs.md
[T-071]: T-071-unconstrained-path-var.md
[T-106]: T-106-keyword-schema-and-value-types.md
[T-112]: T-112-variable-definedness-and-provenance.md
[T-179]: T-179-unknown-host-fires-on-a-host-an-included-file-s-add-host-cre.md
