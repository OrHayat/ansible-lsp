# T-180 — Derive a closed value set from an assert that dominates a use

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P3       | L    | —          |

## Problem

`assert: that: x in ['a', 'b']` states a closed domain for `x` in the plainest way Ansible
has. We do not read it, so every rule that meets a templated value treats it as unknowable —
and says so in three tickets as an aside rather than as work: T-071 lists an `assert` among
the reasons its hint can never be a warning, T-179 gives up on a file whose include is
templated, and T-041 covers only the `meta/argument_specs.yml` half of the same question.

**This is type checking, but not the kind [T-106] does**, and the distinction decides where
it belongs. That epic transcribes *ansible's* closed keyword schema from six `FieldAttribute`
tables — a fixed vocabulary with declared types, where the work is transcription. This is the
domain of a *user's* variable, inferred from their own code: no table to transcribe, nothing
closed in advance, and the hard part is dominance rather than coverage. Same family, different
machinery. It belongs with the "what do we actually know about this name" tickets under
[T-112], where [T-071] already sits, and not under T-106.

The corpus has this everywhere — 107 files carry an `assert`, and the constraining forms are
the ones a reader can use:

```
- container_operation in ['create', 'destroy', 'query', 'set-attr', 'get-attr']
- container_type in ['POSIX', 'HDF5', 'PYTHON']
- spdk_bdev_operation in ['expose', 'unexpose']
- (unbind_nvmes | default('no')) in ['no', 'inventory', 'all']
```

**The complete case, from `~/app`.** `roles/spdk-bdev/tasks/main.yml` includes
`_validate.yml`, which asserts `spdk_bdev_operation in ['expose', 'unexpose']`, then eight
lines later:

```yaml
- ansible.builtin.include_tasks: "_{{ spdk_bdev_operation }}.yml"
```

`_expose.yml` and `_unexpose.yml` both sit in that directory. The domain is closed, the
targets exist, and the author wrote the reasoning in a comment above the line —
*"_validate.yml already asserts spdk_bdev_operation in {expose, unexpose} above, so the
dynamic filename is always _expose.yml or _unexpose.yml."* Our scan reports it under
**TEMPLATED, MATCHES NOTHING**.

That is the shape of the win: not a new diagnostic, but the input several existing rules are
missing.

## Approach

Read the asserts, produce a value set for a variable, and let the existing rules ask for it.

**Recognising the constraint.** The forms above, and only forms that are genuinely closed:
`x in [...]`, `x == 'lit'`, and an or-chain of `==` on one variable. A `!=`, a comparison, a
`| length` test constrain without enumerating and yield nothing. `that:` takes a string or a
list of strings, and a list is ANDed — so each clause is considered on its own and the
intersection is taken when two clauses constrain the same name.

The `| default('no')` wrapper in the corpus sample matters: the asserted expression is not
always a bare name. Unwrapping `default()` is worth doing, since it *widens* the set by the
default value rather than narrowing it — get that backwards and the set is wrong in the
unsafe direction.

**Dominance is the hard half, and it is where this earns its size.** A set is only usable
where the assert has certainly run:

- same play, and the assert's task ordered before the use — the spdk case reaches it through
  an `include_tasks` of `_validate.yml`, so this is a graph question, not a line-number one
- the assert not skipped by its own `when:`, and not in a block the use sits outside of
- what a `failed_when`/`ignore_errors` on the assert does to the guarantee — measure it, a
  run that continues past a failed assert makes the domain a lie

Get dominance wrong and every consumer inherits a false narrowing, which is worse than the
silence they have now. So the default answer is **no set**, and a set is produced only when
the walk can show the assert ran.

**Consumers, none of which change severity because of this** — a wider input, not a new
claim:
- T-071 substitutes each value for the glob, which is the upgrade its own Approach describes
- T-179 stops silencing a file whose templated include has a closed domain
- T-032's static `when:` evaluation gains a source of literal facts

## Traps

- A closed domain says what is *permitted*, not what is *passed*. Every consumer of it is
  still reasoning about a contract, and `-e` can violate one — an assert failing is a normal
  runtime outcome, not proof the value was never other.
- The value may be path-shaped (`../../x`), the same under-approximation T-071 already
  records for its glob.
- Do not let this become a diagnostic of its own without a separate ticket. "This value is
  not in the asserted set" is a different, much stronger claim than anything here.
- **A domain is not a definition.** An `assert` constrains a value; it never sets one. A name
  with a closed domain and no definition anywhere is still undefined, and this must not
  exempt it from `var-undefined` — the set says what the value may be *if* it exists.

[T-071]: T-071-unconstrained-path-var.md
[T-106]: T-106-keyword-schema-and-value-types.md
[T-112]: T-112-variable-definedness-and-provenance.md

## Done when

- [ ] `x in ['a','b']`, `x == 'a'`, and an or-chain of `==` each yield their set, one test
      per form, with a non-enumerating clause (`!=`, `> 3`) yielding none as the control
- [ ] two clauses constraining one name intersect; clauses on different names stay apart
- [ ] `| default('v')` widens the set by `v` rather than narrowing it, pinned by test
- [ ] dominance is measured, not assumed: an assert behind a false `when:`, one after the
      use, and one reached through `include_tasks` each get a probe that could report either
      way, and `ignore_errors` on an assert is run against real ansible
- [ ] the spdk-bdev case above resolves to `_expose.yml` and `_unexpose.yml`, asserted as a
      fixture rather than against `~/app`
- [ ] no rule changes severity as a result; the corpus's TEMPLATED-MATCHES-NOTHING count
      falls and nothing else moves
