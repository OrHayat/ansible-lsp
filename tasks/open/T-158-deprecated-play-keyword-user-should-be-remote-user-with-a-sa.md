# T-158 — Deprecated play keyword: user: should be remote_user:, with a safe autofix

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P3       | S    | T-128 | —          |

## Problem

`user:` on a play is a deprecated alias for `remote_user:`. ansible-core says so in its own
source, and again in the error it raises when both are set — "The use of 'user' is deprecated,
and should be removed" (`play.py:170`).

But on its own it is completely silent. Measured on 2.21.2: a play with `user: alice` and no
`remote_user:` runs clean, exit 0, no deprecation warning from `ansible-playbook` or
`--syntax-check`. So the only time an author is told about it is the one case where they have
already written both — which is the case they are least likely to hit.

We are silent too, and for a reason that is worth writing down: T-110 replicates ansible-core's
**load errors**, and there is no error here to replicate. T-110 row 13 covers the both-set
collision and stops there. Nothing else on the board owns deprecated *keywords* — T-064 is
deprecated plugin/module names, which is a different table.

Why it was deprecated, from the comment above the rename (`play.py:164-166`):

> The use of 'user' in the Play datastructure was deprecated to line up with the same change
> for Tasks, due to the fact that 'user' conflicted with the user module.

That collision is still live and still bites, in a way the play-level rule does not cover. The
same two keys give three different errors depending on context — measured:

| where   | what ansible says                                              |
| ------- | -------------------------------------------------------------- |
| play    | `both 'user' and 'remote_user' are set for this play...`        |
| task    | `conflicting action statements: debug, user` — read as the module |
| block   | `'user' is not a valid attribute for a Block`                    |

Only the first is T-110 row 13's. The task one is row 12's, and it is the deprecation comment's
own reasoning showing up as a confusing error two contexts over.

## Approach

A HINT or WARNING on `user:` at play level when `remote_user:` is **not** also present — the
both-set case is already T-110 row 13's ERROR and must stay that way, so this rule and that one
are mutually exclusive by construction. Its own rule id (`deprecated-keyword`), suppressible
per T-010 and toggleable per T-025, since ansible-core is silent here and this is ours.

**The autofix is the point, and it is provably safe** — unlike its sibling T-156. The rewrite
we would emit is exactly the assignment the loader already performs (`play.py:172-173`):

```python
ds['remote_user'] = ds['user']
del ds['user']
```

Renaming the key produces a document that loads to a byte-identical Play. There is no semantic
delta to reason about, which is the opposite of T-156's `with_<lookup>` → `loop:` rewrite, where
a naive key swap changes iteration count and `item` shape. Worth stating in the code comment,
because "modernization autofix" reads as risky by default and this one genuinely is not.

Severity is the open question. T-156 argues HINT + off-by-default for `with_*` because upstream
explicitly calls it *not* deprecated. Here upstream calls it deprecated in two places, so
WARNING + on-by-default is defensible. Settle it against T-129's triage rather than in
isolation — ansible-lint may already have a rule with a settled severity.

Note the keyword set already models this correctly and needs no change:
`keywords.rs:255` gives `KeyContext::Play` a preprocess-level escape list containing `user`, so
`user:` alone is not reported as an invalid Play attribute today. That escape is exactly the
hook this rule hangs off.

## Done when

- [ ] `user:` on a play, with no `remote_user:`, gets a diagnostic naming `remote_user:` as the
      replacement
- [ ] the both-set case still produces T-110 row 13's ERROR and *only* that — asserted on both
      sides, so neither rule can silently take the other's case
- [ ] a quick fix renames the key in place, leaving the value and any comment untouched
- [ ] `user:` on a task and on a block are untouched — those are row 12's and T-107's, pinned
      by tests so this rule cannot leak into them
- [ ] its own rule id, `# noqa`-suppressible per T-010
- [ ] severity settled against T-129's ansible-lint triage, and the choice recorded here
