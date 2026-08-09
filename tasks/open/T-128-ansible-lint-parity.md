# T-128 — ansible-lint parity

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | epic | P2       | L    | —          |

## Problem

Every user of this language server is already running ansible-lint. We have never
systematically compared the two, so three questions are open and none of them has an answer
on the board:

1. **What do we duplicate?** A rule a user already gets from CI is worth less as a squiggle,
   and possibly worth nothing.
2. **What is a cheap win?** Some of its rules are pure AST shape tests, and we have the AST.
3. **Where should we deliberately differ?** ansible-lint is a CI tool and can afford to be
   strict and noisy. An editor cannot.

The one datapoint that exists is encouraging and narrow: `when-import-var-mutated` is novel —
ansible-lint has no rule for it (`tasks/README.md:246`). That is the shape of the answer this
epic is looking for, generalised: **the rules that need a resolved cross-file graph and a
variable index are ours to have, and are not portable to a per-file linter.**

Deliberately **one child**. Filing per-rule tickets before the triage would be inventing work
— the triage decides how many children this epic gets, and the `port` bucket is expected to
be a minority. If it comes back with three portable clusters, this epic is three more tickets
and done; if it comes back saying we duplicate very little and should port almost nothing,
that is a complete and useful result and the epic closes on it.

Sized L on the assumption that the triage finds real work. If it does not, closing this early
is a success, not a failure.

## Children

- [ ] T-129 — Triage every ansible-lint rule: covered, port, reject, out of scope
- [ ] T-156 — with_<lookup> to loop: modernization, with autofix only where it is provably safe

## Done when

- [ ] T-129 is closed and its buckets are recorded here
- [ ] any `port` clusters are filed as children and closed or rejected
- [ ] `tasks/README.md` gains a short statement of how this project relates to ansible-lint,
      so the next person does not have to re-derive it
