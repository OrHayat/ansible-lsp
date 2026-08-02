# T-059 — Call sites must satisfy the callee's required vars (`var-unpassed`)

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P1       | L    | T-051      |

## Problem

A role/tasks file using `{{ db_name }}` unguarded cannot be checked *in that file* — any
caller may inject the var, so the file alone is undecidable and `var-undefined` (T-051)
rightly never fires there. But each **call site in a playbook** is decidable, exactly like
the playbook-level `{{ region }}` case one level deeper: calling a role imports the role's
unguarded, un-defaulted uses as obligations into the play's closed world.

```yaml
- hosts: dbservers
  roles:
    - role: db            # db's tasks use {{ db_name }} unguarded, no default anywhere
                          # nothing in this play defines or passes it
                          # -> warn HERE, on the call: the fix belongs at the call site
```

The same call with `vars: { db_name: appdb }` is silent. A role's unguarded,
un-defaulted uses *are* its required parameters — this is the inferred-contract version of
T-041's declared `argument_specs`.

## Approach

`definitions_with_deps` already walks exactly the right tree (role defaults/vars,
includes, `set_fact`/`register` in called files) to collect *definitions*. Collect *uses*
along the same walk, then evaluate per call site with the caller's accumulated context.

- Warning span: the role/include **reference** (`roles:` entry, `include_role` name,
  `include_tasks` path) — where the fix goes. Rule id `var-unpassed`, noqa-suppressible.
- A use inside the callee guarded by `is defined` / `default(…)` is the callee declaring
  an *optional* param — exempt, same softening as T-051.
- Satisfied by any of: call-site `vars:`, play `vars:`/`vars_files`, earlier `set_fact`
  in the walk, the callee's own `defaults/`/`vars/`, magic/facts/declared names — the full
  T-051 exemption list, evaluated in the caller's context.
- If the role has `meta/argument_specs.yml`, the declared contract wins — defer to T-041's
  check and stay silent here, so the two never double-report.
- Templated or conditional include targets: skipped, as everywhere.
- Message concedes the opaque sources, same wording family as `var-undefined`:
  "role `db` uses `db_name`, which nothing in this play defines or passes — it may still
  come from inventory, facts, or extra-vars."

## Traps

- **Transitive ordering**: a `set_fact` early in the callee legitimately feeds later tasks
  and deeper includes — the walk must accumulate as it descends, not evaluate per-file.
- **Host-targeted inventory vars** look "unpassed" from the play. Until T-062 indexes
  inventories, expect this to dominate false positives — the corpus gate decides whether
  this ships before or after T-062.
- **Role dependencies** (`meta/main.yml` `dependencies:`) run first and may define vars.
- `roles:` dict-form `vars:`, `include_role` task `vars:`, and include params all count as
  passing — the extraction must see every spelling.

## Done when

- [ ] world A/B pinned: same role, caller passing the var is silent, caller omitting it
      warns at the call site
- [ ] callee-guarded (`is defined`/`default`) uses are optional params — silent, pinned
- [ ] a role with `argument_specs.yml` defers to T-041 — no double report
- [ ] transitive `set_fact` and role `defaults/` satisfy the obligation — pinned
- [ ] corpus gate: near-zero on `~/app/ansible`, every hit inspected and real
- [ ] `# noqa: var-unpassed` works; `scan` prints the list
