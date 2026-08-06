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
rules that describe the wrong runtime are worse than missing ones. T-115 and T-116 are
additive and can follow in either order.

T-116 is the one with reach beyond this epic — it is the model T-037 (vault) needs for its
hedge, T-051 needs to decide severity per keyword, and T-089 needs to decide where a
subscript diagnostic is anchored. It is worth doing before those three rather than having
each invent its own answer.

T-034 is deliberately **not** here: expanding templating that only looks dynamic is about
resolving paths, and it lives with the other file-reference work.

## Children

- [ ] T-115 — Filter, test and lookup name index
- [ ] T-116 — Undefined propagation: the Marker model
- [ ] T-117 — when: is strict since 2.19 — audit condition.rs

## Done when

- [ ] every child is closed or rejected
- [ ] everything derived from ansible-core here records the version it was read from
- [ ] no rule in this epic fires on the corpus without a human confirming the hit is real
