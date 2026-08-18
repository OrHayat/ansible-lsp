# T-188 — Parse Jinja expressions into a tree instead of matching them as strings

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | L    | T-114 | —          |

## Problem

`condition::classify` recognises shapes by cutting strings apart:

```rust
s.split_once(" is ")                       // `x is defined`
s.strip_suffix("> 0")                      // `... > 0`
   .and_then(|l| l.strip_suffix("| length"))
```

Every shape is written against one exact spelling of a construct Jinja will accept in many.
Two bugs already filed are the same defect seen from different sides:

- **T-186** — `r.stdout | length > 0` classifies as `RequiresNonEmpty { var: "r" }` and the
  inlay hint reads *"runs only if r is non-empty"*. False: `r` is a registered result and is
  always non-empty; `r.stdout` is what must be. The extractor stops at the dot because the type
  has nowhere to put the rest.
- **T-187** — `hosts|length>0` classifies as `Unknown` while `hosts | length > 0` classifies
  fine. Identical meaning, different answer, because the matchers only ever see the spaced
  spelling.

Neither is fixable at the string level without adding another special case per spelling. The
root cause is that `r.stdout` is an **expression** — an accessor applied to the variable `r` —
and a `String` cannot represent that. Patching the label or padding the operators treats the
symptom; the next spelling breaks it again.

## Approach

A small expression tree, and `classify` reads the tree instead of the text. Enough of Jinja to
answer the questions the rules actually ask, not a reimplementation:

- **names and accessors** — `r`, `r.stdout`, `r['stdout']`, `r.results[0].stdout`, so the root
  variable and the accessed path are separately available. This is what T-186 needs.
- **filter chains with arguments** — `| length`, `| default('x')`, `| default([], true)`, so a
  shape is recognised by structure rather than by suffix.
- **comparisons and boolean structure** — `> 0`, `== 'x'`, `in [...]`, `and`/`or`/`not`.
- **tests** — `is defined`, `is not defined`, `is skipped`.

Whitespace stops mattering by construction, so T-187 dissolves rather than being fixed.

**Non-goals.** Evaluation is T-035, not this. Statements (`{% for %}`, `{% if %}`, macros) are
not expressions and are out of scope. Full Jinja2 grammar conformance is not the bar — the bar
is that anything not represented parses to a node that classifies as `Unknown`, never a guess.

**Also unblocks**, and worth checking before designing the tree so it serves them too:

- `resolve::substitute_literals` currently requires a bare identifier and abandons the whole
  value on any filter, which is what makes `{{ x | default('lit') }}` fall through to globbing
  (recorded with measurements on T-034)
- T-089 indexed access into static list vars
- T-115's filter index has a natural home once filters are nodes rather than substrings

## Done when

- [ ] an expression tree that distinguishes the root variable from the accessor path, with
      `r`, `r.stdout`, `r['stdout']` and `r.results[0].stdout` each asserted
- [ ] `classify` reads the tree; no `strip_suffix`/`split_once` shape matching remains in it
- [ ] T-186's case is correct or `Unknown` — never a claim about `r` — asserted on both
      `label()` and `requirement()`
- [ ] T-187's case classifies identically to its spaced spelling, plus one non-`RequiresNonEmpty`
      arm asserted the same way
- [ ] a quoted operator survives: `x == "a|b"` keeps its literal value
- [ ] anything unrepresentable is `Unknown`, with a test naming a construct we deliberately do
      not model
- [ ] the T-032 corpus classification rate (**39% of all conditions, 93% of import-level**) is
      re-measured and recorded here. It must not fall; if it does not rise, say so — the tree
      would then be paying for correctness rather than reach, which is still worth it but
      should be stated rather than discovered later.
- [ ] T-114's standing box honoured: no rule in this epic fires on the corpus without a human
      confirming the hit is real
