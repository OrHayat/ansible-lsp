# T-197 — A notify resolved only by a later dynamic include_role is fatal on order alone

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-099 | T-196      |

## Problem

A dynamic `include_role` contributes its handlers to the play, so a `notify:` can legally
reach into a role the play never lists. But the contribution happens **when the include runs**,
not at play compile time, so the same play is correct or fatal depending on the order the
includes are written in. Measured on 2.21.2 — identical roles, identical notify, two lines
swapped:

```yaml
tasks:
  - include_role: {name: beta}    # defines `beta handler`
  - include_role: {name: alpha}   # notifies it        -> RUNNING HANDLER, exit 0
```
```yaml
tasks:
  - include_role: {name: alpha}   # notifies it
  - include_role: {name: beta}    # defines `beta handler`  -> FATAL
```

Nothing about the second is distinguishable from the first by reading either role. The fault
is purely positional, which is exactly the kind a reader does not see and a tool can.

[[T-196]] *suppresses* on any play containing a dynamic `include_role`, because it cannot tell
these apart. This ticket is what buys that suppression back: once the order is modelled, the
bad case is diagnosable and the good case stops being collateral silence.

## Approach

Model handler availability as a position in the play's task order, not a set membership:

- a static contributor (`roles:`, `meta/main.yml` dependencies, the play's own `handlers:`) is
  available to **every** task in the play
- a dynamic `include_role` contributes from **its own position onward**

Then a literal `notify:` whose only match is a contributor positioned after it is provably
fatal, with the same certainty as [[T-196]]'s absent-name case — and the message should say
*why*, naming the include and its line, because "not found" would send the reader hunting for
a typo that isn't there.

Conditional and looped includes are the edge: an `include_role` under a `when:` may not run at
all, so a notify after it is not provably safe either. Treat "may not have run" as unknown
rather than as available — [[T-032]]'s condition analysis already answers "can this be
statically decided", and this is another consumer of it.

`import_role` is static and expands at parse time, so it is not in this ticket's scope — it
belongs to the always-available set above. Verify that rather than assuming it: the
static/dynamic split is exactly where T-166 found the forms disagreeing.

## Done when

- [ ] the two orderings above produce different verdicts, both asserted in one test — an
      assertion on only the failing order cannot show the rule discriminates
- [ ] the diagnostic names the include and its position, not just the missing name
- [ ] a conditional `include_role` yields no verdict either way, with the reason tested
- [ ] `import_role` is measured, and the result recorded here, before it is treated as static
- [ ] [[T-196]]'s dynamic-include suppression is narrowed to what this cannot decide, and its
      test updated to match — the suppression and this rule must not both stay maximal
- [ ] `# noqa` suppressible, per T-010
- [ ] demo fixture with both orderings, per rule 4
- [ ] seen red before the fix, per rule 5
- [ ] corpus gate: count how many plays contain a dynamic `include_role` at all — if the answer
      is near zero, say so, because that bounds this rule's whole value
