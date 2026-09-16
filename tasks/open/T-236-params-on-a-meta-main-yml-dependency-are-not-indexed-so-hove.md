# T-236 — Params on a meta/main.yml dependency are not indexed, so hover and go-to-definition go silent on them

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | S    | T-112 | —          |

## Problem

A param on a play's `roles:` entry is a variable definition we index. The same param on a
role's `meta/main.yml` `dependencies:` entry is not, although Ansible treats the two the same
way — both entries are `RoleInclude` objects (`metadata.py:59-62`), and T-164 measured that a
dependency's params reach the role as variables on 2.21.3.

Measured through the real `ansible-lsp` binary (`textDocument/hover` and
`textDocument/definition`, cursor on `app_env` inside the braces), same entry in two files:

```yaml
# play.yml
- hosts: all
  roles:
    - role: web
      app_env: staging
      app_dir: "/etc/{{ app_env }}"
```

```yaml
# roles/app/meta/main.yml
dependencies:
  - role: web
    app_env: staging
    app_dir: "/etc/{{ app_env }}"
```

| file                      | hover                             | definition   |
| ------------------------- | --------------------------------- | ------------ |
| `play.yml`                | "role param … = `staging`"        | `play.yml:4` |
| `roles/app/meta/main.yml` | `null`                            | `null`       |

Found while doing T-164, which added the `role-param-not-keyword` diagnostic for these
entries — so the diagnostic now sees them and the variable index still does not.

## Approach

`vars.rs` reads params only from `Play.roles` (`for param in &r.params`, around
`vars.rs:681`). T-164 added `ast::dependency_roles`, which builds the same `RoleUse` values
for a `meta/main.yml`, so the extraction exists; the definitions walk needs to read it for a
role's own meta file, with the same entry-scoped `scope` the `roles:` params carry (T-100's
scope rule — a param is visible within its entry and to the role, not elsewhere).

Enumerate the readers of the index before choosing where this goes (working rule 3): hover,
go-to-definition, the `ansible/references` variable paint, `path_substitution_hover`, and
`var-undefined`. Only hover and definition are measured above.

Out of scope, and unmeasured: whether a dependency's params should be visible from the
*dependency role's* files (`roles/web/tasks/main.yml` reading `app_dir`). The `roles:` side
has the same question; answer both together if it comes up.

## Done when

- [ ] hover and go-to-definition on a param use inside a `dependencies:` entry find the param
      on the same entry, through the real handler
- [ ] the same name on a *different* dependency entry is not offered — the scope control
- [ ] every other reader of the index listed above is checked and asserted for this shape
- [ ] the `play.yml` half of the table above stays as it is
