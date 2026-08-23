# T-207 — A re-included vars file collapses to one precedence level, and the index keeps the one not in effect

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P2       | M    | —          |

## Symptom

When the same file is loaded twice at two precedence levels, the index keeps one of them, and
it keeps the **lower** one — the value that is not in effect.

The shape is the one T-184 is about: a role's `tasks/main.yml` opening with
`include_vars: main.yml`. Ansible loads that file twice — once automatically as role vars
(precedence 15), once through the include (18) — and the include is what wins against a
task-level `vars:` at 17. We report only the role-vars one.

Measured, in `a_reinclude_of_the_roles_own_vars_is_indexed_at_both_precedence_levels`
(`vars.rs`) — `#[ignore]`d per rule 7, so it states the answer we owe rather than the one we
give. Run it with `--ignored` to see the failure:

| definitions of `thing` | source reported |
| ---------------------- | --------------- |
| ours                   | `RoleVars` only |
| what Ansible does      | both, `IncludeVars` in effect |

So a hover on such a name says **role var** when the effective source is the include, and any
rule that reasons about precedence from the index reasons from the wrong level.

## Cause

`dedup` (`vars.rs:983`) keys on `(name, file, span.start)` and ignores `source`. Two loads of
one file produce the same name at the same span in the same file, differing only in the source
that pulled them in, so the second is dropped and whichever came first survives.

The control that proves it is the lookup and not the collapse: an include naming a vars file
that is **not** auto-loaded — no collision — does index `VarSource::IncludeVars` correctly.
That is `include_vars_of_a_bare_name_in_a_role_indexes_the_role_vars_file`, in the same module.

Found while fixing T-206. It was invisible before that, because the include used to resolve to
the wrong file entirely and never reached this collision at all.

## Fix

**Keep both definitions — do not collapse to the winner.** This was considered and rejected:
`dedup` could keep the highest-precedence entry instead of the first, which is one comparison
and fixes "which value is in effect" everywhere. It is the wrong fix.

Hover already renders every definition as a precedence-ordered stack — `defs.sort_by` on
`source.precedence()` (`main.rs:1981`), `multiple = defs.len() > 1` (`1987`), and a
`← effective` marker on the first row (`2024`). The stack exists to show a reader what else
defines the name and which one wins. Collapsing to the winner deletes a row from a list whose
whole purpose is completeness, and it deletes the row that would carry the marker.

It is also worse than today for [[T-184]]: one row reading `include_vars` gives no hint that
the file is auto-loaded as well, when "this is loaded twice and your re-include is why" is the
entire thing worth telling the author. The two-row stack says that on its own, before any
diagnostic is written.

So the work is the expensive one, and the ticket stays `M`.

Not simply "add `source` to the dedup key" — that changes what every consumer sees, and the
comment at `vars.rs:1054` records a deliberate reason for keeping the first of two routes to
the *same* definition (Ansible compiles both and skips the second per host, so the first is the
one that runs). That reasoning is about one definition reached twice; this is two different
loads at two levels, and telling them apart is the actual work.

Enumerate the readers before choosing, per rule 3 — hover, `undefined_uses`, inlay hints and
the precedence-ordering code all read `Located::source`, and letting duplicates through will
change at least the first and third.

**Measure first:** whether a `RoleVars`/`IncludeVars` pair is the only collapsing combination,
or whether `vars_files` re-reads and `include_vars` of the same file from two tasks do it too.
Nobody has run that.

## Done when

- [ ] a name loaded at two precedence levels is indexed at both, with the effective one
      identifiable
- [ ] hover on such a name renders both rows, `include_vars` first and marked `← effective`,
      with the role-vars row beneath it — the stack is why collapsing was rejected, so it is
      the assertion that proves the right fix landed
- [ ] `a_reinclude_of_the_roles_own_vars_is_indexed_at_both_precedence_levels` passes with its
      `#[ignore]` attribute deleted — it already asserts the right answer, so the fix is done
      when it goes green, and nothing about it needs rewriting
- [ ] hover on such a name names the level that is in effect
- [ ] the deliberate first-route-wins behaviour at `vars.rs:1054` still holds, asserted — this
      fix must not re-open the case that comment is about
- [ ] the other collapsing combinations measured and either covered or recorded as out of scope
- [ ] seen red before the fix

**Turned up by:** [[T-206]]. **Blocks the full answer for:** [[T-184]], which reports exactly
this precedence lift.
