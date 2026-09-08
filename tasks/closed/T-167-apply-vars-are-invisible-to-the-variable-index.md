# T-167 — apply: vars: are invisible to the variable index

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P3       | S    | T-113 | T-020      |

## Problem

`apply:` on a dynamic include takes task keywords, `vars:` among them, and they reach every
task the include brings in. Live-verified on 2.21.2:

```yaml
- include_tasks:
    file: inc.yml
    apply:
      vars: {applied_var: from_apply}
```

`inc.yml` reads `{{ applied_var }}` and gets `from_apply`. We index none of it.

Split off T-166, which landed the `apply: {when:}` half. The two shared one walk but not one
consumer: the condition feeds `when-import-var-mutated`, where absence was a *wrong fact*
(`conditional=false` on a guarded include). This half feeds only the variable index.

## Why it is P3, and why it waits for T-020

Nothing we say today is wrong because of it — checked, not assumed:

- it cannot make a path resolvable, so it can never cause a false missing-file. Measured:
  `file: "{{ which }}.yml"` with `apply: {vars: {which: alpha}}` fails in ansible itself
  with `'which' is undefined`. The apply vars never reach the include's own templating.
- it cannot cause a false `var-undefined`: that rule is playbook-only and `apply:` targets
  task files, which are never checked.
- the vars do not leak past the block, so a later task referencing one is *correctly*
  undefined.

So the only cost is hover and go-to-definition on those names, inside the included file.

And that is exactly why it needs T-020 first. These vars belong to the *included file*, not
to the playbook that writes them. Indexing them at play level would repeat the scope error
T-100 fixed — a definition filed under a file where it is not usable, so hover confidently
answers where Ansible would fail. There is no correct home for them until the invocation
chain exists. `Located::scope` is the mechanism that will express it.

## What the measurements settled

All on 2.21.2, before any of it was written (rule 1). Each collision was run from both
sides so a wrong hypothesis would have shown a different value (rule 2):

| probe                                                  | result                    |
| ------------------------------------------------------ | ------------------------- |
| read inside the included file                          | reaches                   |
| read in a further include *nested* inside that file    | **also reaches**          |
| read in the calling play after the include returns     | undefined                 |
| `include_role` + `apply: vars:`, read in the role      | reaches                   |
| vs the role's `vars/main.yml` (15)                     | apply wins                |
| vs task `vars:` (17), `include_vars` (18), `set_fact` (19), include params (21) | apply loses |
| through `hostvars[h]`, with a live `set_fact` control  | invisible                 |
| supplying a templated include path *inside* the target | **works**                 |
| supplying an `import_playbook` under an apply include  | impossible — parsed as a task, "does not support raw params" |
| `apply:` on `import_tasks` / `import_role`             | **hard error** — "Invalid options for import_tasks: apply", nothing runs |
| `apply:` on a play's `roles:` entry                    | silently inert — the role read the name UNDEF while a variable called `apply` held the whole mapping |

So it is block vars, level 16, exactly as "`apply:` is a Block" predicts.

## Outcome

`Reference.apply_vars` extracts the entries the way T-166 extracts `apply: when:`; they ride
the T-020 edge as `reverse::ApplySite` (shared behind one `Arc` per reference, so a templated
include reaching twenty candidates carries one copy); and `apply_var_definitions` turns the
inbound edges of a file into `VarSource::ApplyVars` definitions. Appended in
`cached_definitions`, which every one of the eight readers of the definitions list comes
through — the T-100 lesson about a rule that reaches one consumer and not another (rule 3).

- `file`/`span` point at the caller, so a jump lands on the value; `via` carries the include
  edge so a hover can say how the definition got here; `condition` carries the include's own
  `when:`, since a guarded include binds these only where it holds.
- `scope` is deliberately `None`. The scoping is structural — the definition only ever enters
  the *included* file's list — and the range that would be right, "the whole of the included
  file", is not something a span in `self.file` can express. Filling it with the `apply:`
  block's span would be inert everywhere except a self-include, where it would be wrong.
- Three `VarSource` arms placed by measurement rather than by default: precedence 16,
  invisible to `hostvars`, and *usable* for path substitution (that last one is why the
  match mattered — defaulting it to false would have silently dropped a navigable target).
- Label is `apply: vars`, not `block var`: the block is in another file and that name leads
  back to nothing.
- **`apply:` is read only on `include_tasks` and `include_role`.** A comment already in
  `references.rs` claimed an import's `apply:` "never reaches here (T-101)"; measured against
  our own extractor, it did — an `ImportTasks` reference came back holding the entries. That
  is rule 1's second half, a claim about our own tool that was never run. Reading them would
  answer a hover for a name that never binds, on a playbook Ansible refuses to start. Gated
  once at the `apply` lookup, so `apply: when:` (T-166) is fixed by the same change rather
  than left on a second rule.
- **A `roles:` entry's `apply:` binds nothing**, and is not an error either — it is T-100's
  shape, an unknown key becoming a role param. Measured: the role read `applied_var` as UNDEF
  while a variable literally named `apply` held `{'vars': {'applied_var': 'from_apply'}}`.
  Entries reach `Reference` by a different path from tasks, so this already held; pinned by
  `a_roles_entry_apply_binds_nothing`, watched red with that path made to carry them.
  (`apply` is already on T-100's warn list, so the key itself is not silent to a user.)

Measured cost: none. The lookup is one index read, no file touched. On the 2,100-file tree,
open 524 -> 493 ms and close 4 -> 4 ms against the pre-T-020 binary.

## Documented misses, each silence rather than a wrong answer

- **Transitive reach.** An apply var does reach a further include nested inside the target
  (measured above), and we answer only for the direct call site. Walking it would mean
  climbing inbound edges with a cycle guard and multiplying entries per ancestor;
  `demo/tasks/apply_deeper.yml` is the fixture that records the gap.
- **The workspace scan passes `None`.** The graph is still being built while it runs, so an
  answer from it would depend on file order. Costs the scan the path-substitution targets an
  apply var could supply; costs no diagnostic, since templated references never warn.

## Done when

- [x] a task file's `{{ applied_var }}` resolves to the `apply: vars:` entry at its call
      site, with the value span as the jump target
- [x] the definition is scoped to the included file, not to the playbook that writes it —
      a use in the *calling* play's tasks stays undefined, asserted
- [x] a demo fixture carries it beside `demo/mutated_conditions.yml`
