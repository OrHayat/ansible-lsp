# T-166 — when-import-var-mutated covers import_playbook only, and four more constructs flip the same way

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P1       | M    | T-121 | —          |

## Problem

`when-import-var-mutated` fires on exactly one thing — `r.kind == ReferenceKind::ImportPlaybook`
(`scan.rs:107`, `main.rs:334`). The hazard it exists for is not specific to
`import_playbook`: it belongs to **every construct whose `when:` is copied onto the tasks it
brings in and re-evaluated per task**. Four more constructs do that, all measured on 2.21.2
against the same fixture — an included file that runs a task, `set_fact`s the variable its
own condition reads, then runs two more:

| construct | `when:` semantics | flips | covered |
| --------------------------------- | ---------------------------------- | ----- | ------- |
| `import_playbook` + `when:` | copied onto every task, per-task | yes | **yes** |
| `import_tasks` + `when:` | same | **yes — 2 tasks skipped** | no |
| `import_role` + `when:` | same | **yes — 2 tasks skipped** | no |
| `roles:` entry + `when:` | same | **yes — 2 tasks skipped** | no |
| `include_*` + `apply: {when:}` | Block-wrapped, inherited per task | **yes — 2 tasks skipped** | no |
| `include_*` + plain `when:` | gates the include itself, once | **no — 0 skipped** | n/a |

The last row is the boundary and it holds: a dynamic include's own `when:` is evaluated once,
deciding whether to include at all, so nothing flips. That is what keeps this rule precise
rather than "any condition near an include".

Same cost as the original: the file half-executes, everything after the assignment silently
skips, and `set_fact` is per-host so a cluster can split. The shipped rule found a real
production break this way (`playbooks/lustre-deploy-full.yml`, per `mutation.rs:9-11`); there
is no reason the other four spellings are rarer in the same repos.

## The apply: case needs extraction first

The other three carry an ordinary task-level `when:`, already on the `Reference`. `apply:`
does not: measured,

```
REF IncludeTasks value=inc.yml conditional=false conditions=[]
```

for an include whose `apply: {when: …}` demonstrably skipped its tasks. So we assert
`conditional = false` about a guarded include — a wrong fact, not just a missing one, and
wrong for anything downstream that asks "does this include always happen".

`apply:` is a nested task-keyword context and nothing descends into it today. Its `vars:` is
invisible for the same reason (measured: `apply: {vars: {x: 1}}` defines `x` for the included
tasks and the index does not have it). That half is precision only — `undefined_uses` is
playbook-only and `apply:` targets task files, so it produces no false diagnostic — but one
walk fixes both and they should land together.

## Approach

- Widen the rule's kind test from `ImportPlaybook` to every construct in the table. The
  cross-file half (`mutation::mutated_vars_in` over the resolved target) is unchanged — only
  the gate on which references are eligible.
- A `roles:` entry already carries its `when:` in the AST; `RoleUse` needs the condition
  spans plumbed the way `Reference` has them, so the diagnostic lands on the condition rather
  than the role name.
- Extract `apply: {when:}` into the reference's `conditions`/`condition_span`, so both this
  rule and [`crate::condition`] see it. Its span is inside the module args, which no
  condition currently is — check that the `# noqa` line lookup still lands on the right line.
- Keep the plain-`when:`-on-a-dynamic-include case **out**, with a test asserting it stays
  silent. It is the only reason the rule can claim precision.

## Progress

The eligibility rule is generic now. `Reference::when_propagates` says whether a
reference's `when:` is copied onto what it brings in, set where the action name is still in
hand — `include_role` and `import_role` share one `ReferenceKind`, so the kind could never
have answered it. The two hard-coded `kind == ImportPlaybook` gates (`scan.rs`, `main.rs`)
now read that flag, so a construct is covered by being classified correctly rather than by
being added to two call sites.

`roles:` entries carry their `when:` for the first time (`RoleUse::when`), which is what
made the third row work.

Verified end to end on a fixture with all four spellings against one mutating target:

```
site.yml:4   roles: entry    reported
site.yml:7   import_tasks    reported
site.yml:9   import_role     reported
site.yml:11  include_tasks   absent — correct, it is evaluated once
```

Corpus: `~/app/ansible` reports **1**, the same `lustre-deploy-full.yml:247` the rule already
found. The widening surfaced nothing new there and nothing false — worth recording as a
measurement rather than as a clean bill, since the fixture proves the new rows do fire.

Left: the `apply:` row, which is the half needing extraction rather than classification,
and its `vars:` companion. The demo fixture is unwritten.

## Done when

- [x] `import_tasks`, `import_role` and a `roles:` entry gated on a variable their target
      assigns each warn, with the same message and rule id as the `import_playbook` case
- [ ] `apply: {when: …}` reaches `Reference::conditions` and `condition_span`, asserted
- [ ] an `include_*` with `apply: {when: …}` gated on a mutated variable warns
- [x] a plain `when:` on a dynamic include stays silent, asserted — it is evaluated once
- [ ] `apply: {vars: …}` lands in the variable index, with the value span as the target
- [ ] `# noqa: when-import-var-mutated` suppresses each new spelling, including the one
      whose condition sits inside `apply:`
- [ ] a demo fixture carries all five flipping constructs and the non-flipping one
- [x] corpus gate: the new spellings reported on `~/app/ansible` are inspected, not assumed
      clean — the shipped rule found a real break, so new hits are the expected outcome
