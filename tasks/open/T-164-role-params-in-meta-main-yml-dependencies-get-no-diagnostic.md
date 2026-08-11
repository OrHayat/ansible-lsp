# T-164 — Role params in meta/main.yml dependencies get no diagnostic

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | S    | T-099 | T-147      |

## Problem

T-100 landed the `role-param-not-keyword` rule on a play's `roles:` entries. A
`meta/main.yml` `dependencies:` entry is the **same object** — `_load_dependencies`
"returns a list of RoleInclude objects" (`metadata.py:59-62`), so it runs the same
`_split_role_params` — and gets nothing.

Live-verified on 2.21.2:

```yaml
# roles/app/meta/main.yml
dependencies:
  - role: web
    tasks_from: alternate.yml    # does not load alternate.yml
    becom_user: root             # does not become anyone
```

Role `web`'s `tasks/main.yml` runs, and the role sees `tasks_from=alternate.yml` and
`becom_user=root` as variables. Identical to T-100's repro, one file over.

## Approach

The rule and its near-miss derivation already exist (`attributes::role_param_problems`,
`KeyContext::RoleDefinition`); what is missing is a caller. `meta/main.yml` parses as
`Ast::Other`, so `attributes::problems` never looks at it — which is exactly the routing
T-147 builds, hence the dependency rather than a second path-sniffing site.

`references::meta_dependencies` already reads these entries and already handles the
`name:` alias, so the extraction half is done; it needs the param split beside it.

Watch the galaxy requirement form (`metadata.py:70-80`): a `dependencies:` entry may be
`{src: …, version: …, scm: …}` rather than a role name. Those keys are `RoleRequirement`
fields, not role params. None of them names a keyword, so `param_suspicion` returns `None`
and they are silent today — confirm that rather than inherit it by luck.

## Done when

- [ ] `tasks_from:` on a `meta/main.yml` `dependencies:` entry warns, same rule id and
      message shape as the play-level case
- [ ] a genuine dependency param does not warn, and neither does the `src:`/`version:`
      galaxy form
- [ ] a `dependencies:` key in a file that is not a role's `meta/main.yml` is untouched
- [ ] a demo fixture carries both halves, beside `demo/role_params.yml`
