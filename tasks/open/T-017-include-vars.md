# T-017 — `include_vars`

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | M    | —          |

## Problem

30 references, all dead. Task-level, and it has more forms than the other include kinds:

```yaml
- include_vars: x.yml                       # bare string
- include_vars: { file: x.yml }             # file:
- include_vars: { dir: vars/, extensions: [yml] }   # a DIRECTORY, not a file
```

Many are templated, so most of the value comes through the glob path.

## Approach

`ReferenceKind::IncludeVars`. Search order: role `vars/` -> role dir -> playbook dir.

The `dir:` form is the one that needs a decision rather than code: it loads *every* matching
file in a directory. Navigation should resolve to the directory itself, and the existence check
should test that the directory exists — **never** that any particular file inside it does.
Treating `dir:` like `file:` would warn on every correct use of it.

Also worth knowing: `include_vars` is a task, so it can carry `when:`/`loop:`. That's already
captured by `TaskContext` and needs nothing new.

## Done when

- [ ] bare, `file:` and `dir:` forms all handled distinctly
- [ ] `dir:` checks directory existence only, and navigates to the directory
- [ ] templated forms glob without warning
- [ ] corpus gate: zero new warnings
