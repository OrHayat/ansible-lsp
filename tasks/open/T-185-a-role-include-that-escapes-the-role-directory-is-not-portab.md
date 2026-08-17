# T-185 — A role include that escapes the role directory is not portable

| Status | Priority | Size | Refs |
| ------ | -------- | ---- | ---- |
| open   | P3       | S    | T-184 |

## Problem

```yaml
# roles/ad/tasks/join.yml
- name: Phase 2.6 - Select first available AD node
  include_tasks: ../../playbooks/tasks/select-available-node.yml
```

The role reaches out of itself and up into the repo. It resolves — the file is there, and we
navigate to it fine — so nothing is wrong *today*. What is wrong is that the role now depends
on the layout above it: it cannot be moved, vendored, or packaged into a collection without
the include breaking, and the breakage appears at run time in someone else's tree.

Real instance: `roles/ad` in `~/app/ansible`, which reaches
`playbooks/tasks/select-available-node.yml` from two files. The same repo shows the cost
already — a second call site routes through a variable
(`ad_select_node_task: "../../playbooks/tasks/..."`) rather than the literal path, so one
shared file is reached two different ways.

## Approach

A hint. The code is correct Ansible and this is a portability judgement, so an error would be
wrong and a warning is probably too loud.

Fires when: the file is inside a role, the reference is a task/vars include, and the
**resolved** target is not under `ctx.role_dir`. Resolved, not textual — `../` counts are
already normalised by the resolver, and a textual `..` check would both miss
`{{ role_path }}/../x` and fire on paths that normalise back inside the role.

Deliberately out of scope, because each is legitimate and would make this noise:

- a playbook including anything (only role-internal files are judged)
- `include_role` / `import_role` naming another role — that is how roles compose
- a target inside a *sibling* role reached via `roles_path`, which is a real if untidy pattern
- collection-qualified references, which never resolve to a workspace path anyway

Message should name the consequence — the role cannot be relocated — not "you used `..`".

## Done when

- [ ] fires on a role file whose include resolves outside its own role dir, silent when the
      target normalises back inside it (`tasks/../tasks/x.yml`), with both in one fixture
- [ ] silent for every out-of-scope case listed above, one assertion each rather than a
      single combined case
- [ ] `# noqa` works, rule id matched exactly
- [ ] corpus gate: count the hits on `~/app/ansible` before shipping and read them. If the
      escape is common there, this is a hint at most and possibly not worth having — record
      the count in this ticket either way
