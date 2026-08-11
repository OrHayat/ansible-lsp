# T-167 — apply: vars: are invisible to the variable index

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P3       | S    | T-113 | T-020      |

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

## Done when

- [ ] a task file's `{{ applied_var }}` resolves to the `apply: vars:` entry at its call
      site, with the value span as the jump target
- [ ] the definition is scoped to the included file, not to the playbook that writes it —
      a use in the *calling* play's tasks stays undefined, asserted
- [ ] a demo fixture carries it beside `demo/mutated_conditions.yml`
