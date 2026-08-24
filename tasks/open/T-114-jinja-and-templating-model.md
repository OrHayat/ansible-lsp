# T-114 — Jinja and templating model

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | epic | P2       | L    | —          |

## Problem

Every diagnostic this project ships stops at the `{{`. We know which variables a template
*mentions* (T-049) and nothing about the language around them — which filters exist, what an
undefined value does as it flows through a chain, or what a conditional is allowed to
evaluate to.

Three children, and they are one epic because they share a source and a hazard. The source is
`lib/ansible/_internal/_templating/` plus the plugin directories; the hazard is that this is
the fastest-moving part of ansible-core. The 2.19 data-tagging rewrite changed conditional
semantics outright (T-117) and replaced the undefined machinery wholesale (T-116), and both
`DEFAULT_UNDEFINED_VAR_BEHAVIOR` and `DEFAULT_JINJA2_NATIVE` are deprecated no-ops in the 2.22
tree. Anything built here has to record which version it was read from, or it will quietly
describe a runtime nobody is using.

Ordering: **T-117 first.** It is a correctness audit of rules that already ship, and shipped
rules that describe the wrong runtime are worse than missing ones. Then **T-115 before T-116** —
see the parser note below, which is the reason they are no longer interchangeable.

T-116 is the one with reach beyond this epic — it is the model T-037 (vault) needs for its
hedge, T-051 needs to decide severity per keyword, and T-089 needs to decide where a
subscript diagnostic is anchored. It is worth doing before those three rather than having
each invent its own answer.

### The Jinja parser question

Nothing in this project parses Jinja. `condition.rs` is character-level string surgery — a
quote-aware word scanner plus a dozen helpers that peel parens, `not`, filters and defaults off
a string. The recurring question is whether to build a real expression parser and AST.

The 2.19 boolean-result check answers it, because it splits into bands that need different
things. Measured live on ansible-core 2.21.2, as `changed_when:` with `s: hello`, `n: [1,2]`:

| Shape                                        | Runtime | Needs to catch statically  |
| -------------------------------------------- | ------- | -------------------------- |
| `"'bad'"` — a bare literal                    | fatal   | nothing; a string check    |
| `n \| length` — an int result                 | fatal   | filter return types, T-115 |
| `s and s` — `and` returns its operand, not a bool | fatal   | AST + operand types        |
| `n` — a variable holding a list               | fatal   | variable type inference    |
| `s \| bool`, `s == 1`, `1 < 2 < 3`            | ok      | —                          |

A parser on its own buys little: it produces a tree nobody can type until T-115 exists. That is
the argument for T-115 before T-116, and for deferring the parser until T-115 is in hand — the
mechanism after the vocabulary, not before it. The literal case needs no parser at all and is
T-117's.

The hazard is `s and s`. Jinja's `and` returns an operand rather than a boolean, so `when: a and
b` is fatal unless both sides are already booleans — a shape that is everywhere in the corpus.
Any rule in that band built before T-116's type model will fire on working playbooks, which is
exactly what the third done-when box exists to prevent.

T-034 is deliberately **not** here: expanding templating that only looks dynamic is about
resolving paths, and it lives with the other file-reference work.

## Children

- [ ] T-115 — Filter, test and lookup name index
- [ ] T-116 — Undefined propagation: the Marker model
- [x] T-117 — when: is strict since 2.19 — audit condition.rs
- [x] T-141 — Condition rules only see when:, not the other four expression keywords
- [ ] T-188 — Parse Jinja expressions into a tree instead of matching them as strings
- [ ] T-211 — The default filter's short and FQCN spellings never classify

## Done when

- [ ] every child is closed or rejected
- [ ] everything derived from ansible-core here records the version it was read from
- [ ] no rule in this epic fires on the corpus without a human confirming the hit is real
