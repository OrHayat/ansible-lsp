# T-099 — Provably-wrong config the editor is silent about

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | epic | P1       | L    | —          |

## Problem

Nine cases where a file on disk is provably wrong and this editor says nothing. They differ
in what *Ansible* does about it, and that difference is the point:

- **Ansible is silent too, forever.** `roles:` typos become variables (T-100). `notify:` a
  handler that does not exist is not checked unless the task reports `changed` (T-028).
  `hostvars[x]` cannot see play vars, so the reference is always undefined (T-104).
  `delegate_to:` fabricates unknown hosts (T-105). Nothing at runtime will ever tell the
  user — the diagnostic is the only chance.
- **Ansible warns, and it scrolls past.** Duplicate mapping keys (T-102). A template in a
  `static=True` field (T-103).
- **Ansible fails hard, but only once you run it.** Keywords rejected on dynamic includes
  (T-101), invalid `vars_files` entries (T-087), out-of-bounds indexing (T-089). Here the
  value is timing: a squiggle now instead of a failed play later.

They are one epic because they share the shape of the fix — a rule over the AST with a closed
legal set, no new index, no new resolution — and because the exemption discipline has to be
consistent across them. Each carries its own ansible-core `file:line`.

Not in scope: keyword *schema* checking, which is T-106's epic. The line between them is that
these nine are individually-argued rules, while T-106 is one table applied uniformly.

## Children

- [ ] T-028 — `notify:` -> handler resolution
- [ ] T-087 — Invalid vars_files entry: provably fatal at runtime, silent in the editor
- [ ] T-089 — Indexed access into static list vars: no element support, out-of-bounds unflagged
- [x] T-100 — Unknown key in a roles: entry silently becomes a variable
- [x] T-101 — Dynamic includes reject keywords imports accept
- [x] T-102 — Duplicate YAML mapping key
- [x] T-103 — A static field carrying a template is used literally
- [x] T-104 — hostvars cannot see play, role or task vars
- [ ] T-105 — delegate_to: empty template, and hosts not in inventory
- [ ] T-136 — A var an import_playbook needs is defined by a source that cannot reach it
- [x] T-155 — loop_control with no loop is dead config, and ansible never says so
- [ ] T-157 — A block or import_tasks in handlers: makes the handler's name unnotifiable
- [ ] T-164 — Role params in meta/main.yml dependencies get no diagnostic
- [x] T-165 — A loop on meta: is silently discarded

## Done when

- [ ] every child is closed or rejected
- [ ] each shipped rule is `# noqa`-suppressible, per T-010
- [ ] the corpus scan reports a hit count per rule, so a rule that fires on nothing real is
      visible as such before it ships
