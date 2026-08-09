# T-088 — Unknown play keyword: Ansible refuses the play, the editor says nothing

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| done   | P3       | S    | T-106 | —          |

## Problem

Live-verified on 2.21.2 during T-016 (a `vars_file:` typo for `vars_files:`):

```
ERROR! 'vars_file' is not a valid attribute for a Play
```

Ansible dies at load time, before any task. The editor stays silent: the YAML parses, the
unknown key lands in `Play.directives`, and nothing checks it. Every misspelled play
keyword — `vars_file`, `role` for `roles`, `task` for `tasks` — is this case.

## Approach

Play-level keywords are a **closed set**, already carried in `keywords.rs` (T-045, from
Ansible's own FieldAttributes). A directive whose key is in neither the keyword schema nor
the structurally-consumed list is provably fatal — ERROR diagnostic on the key span, and
the near-miss machinery can suggest the correction ("did you mean `vars_files`?", the
T-060 edit-distance idea, but against a ~40-word dictionary instead of variable names).

Scope guard: plays only. Task-level keys mix keywords with module names (dynamic, open
set), so the same check there is a different, riskier ticket.

## Done when

- [x] an unknown play-level key gets an ERROR diagnostic on the key span
- [x] a one-edit near miss suggests the intended keyword (`vars_file` →
      "did you mean 'vars_files'?"; distance 1 incl. transposition, per-context dictionary)
- [x] every valid demo play stays diagnostic-free (demo-wide sweep test, plus a
      731-file sweep of a real tree: zero findings)
- [x] `# noqa` suppresses it

Closed by T-107, which delivered the whole per-context rule — the scope guard above
turned out unnecessary: module names and `with_*` are excluded in `ast::build`, so the
task-level check shipped safely at the same time.
