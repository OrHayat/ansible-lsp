# T-123 — Module and role contracts

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | epic | P2       | L    | —          |

## Problem

We resolve *that* a module or role exists. We say nothing about whether it is being **called
correctly** — required parameters, unknown parameters, types, or what a `register`ed result
contains. Five tickets, one thesis: a callable has a declared signature, and both sides of
every call site are checkable against it.

Two halves that meet in the middle:

**Modules.** T-046 replaces the best-effort `find_action` with real `ModuleArgsParser`
semantics — `FREEFORM_ACTIONS` vs `RAW_PARAM_MODULES`, `parse_kv`, the `args:` merge
(`parsing/mod_args.py:295-371`). T-057 then reads `DOCUMENTATION`/`RETURN` for the schema,
and T-058 says so when a module ships neither. This is the board's only 3-deep chain —
T-046 → T-057 → T-058 — and doing them out of order is not possible: you cannot check an
argument you have not correctly split from the module name.

**Roles.** T-041 parses `meta/argument_specs.yml`; T-063 ports the full `include_role` /
`import_role` argument surface (`VALID_ARGS` at `role_include.py:40-43`).

The role half has a silent fault found in the 2.22 audit that belongs to **T-041**, not to a
new ticket:

> `tasks_from: alternate.yml` loads the tasks fine — because `''` is probed first for a
> `*_from` — but argument-spec validation is keyed on the **verbatim** string
> (`role/__init__.py:360-361`) while `argument_specs.yml` entry points are bare names. So
> `tasks_from: alternate.yml` silently skips validation entirely, and `tasks_from: alternate`
> does not. Also silent: a spec entry point with no matching tasks file, and a tasks file with
> no spec entry.

Related and worth stating so nobody re-derives it: `meta/argument_specs.*` **wins over**
`meta/main.yml`'s `argument_specs:` — not merged (`role/__init__.py:317-343`) — and a missing
top-level `argument_specs:` key yields `{}` with no warning.

Completion is the payoff both halves want and neither can have: there is no completion
provider today (T-124's epic). This one supplies the data; that one supplies the protocol.

## Children

- [ ] T-041 — `meta/argument_specs.yml` role signatures
- [ ] T-046 — Harden the module/args split (ModuleArgsParser semantics)
- [ ] T-057 — Parse module `DOCUMENTATION` / `RETURN` for input & output schema
- [ ] T-058 — Warn when a module ships no `DOCUMENTATION` / `RETURN`, with `# noqa` for legacy
- [ ] T-063 — Port the full `include_role` / `import_role` parameter surface
- [ ] T-149 — Validate meta/argument_specs.yml itself, not just call sites
- [ ] T-153 — Playbook .meta files: the playbook-level argument_specs

## Done when

- [ ] every child is closed or rejected
- [ ] the `tasks_from` extension trap above is covered by a test in T-041
- [ ] a wrong or missing required parameter is diagnosed at the call site, for both a module
      and a role
