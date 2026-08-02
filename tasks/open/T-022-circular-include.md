# T-022 — `circular-include` warning

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P3       | S    | T-020      |

## Problem

`a.yml` includes `b.yml` includes `a.yml`. With `include_tasks` this is a runtime infinite
recursion; with `import_tasks` Ansible errors at parse time. Neither is caught by anything
today, and the runtime version fails partway through a run.

Unknown whether the real repo has any — the reverse index will answer that. If it has none,
this is cheap insurance rather than a fix.

## Approach

Cycle detection over T-020's graph, following only edges that actually recurse at runtime:
`include_tasks`, `import_tasks`, `include_role`/`import_role` and `meta` dependencies.
`import_playbook` too, since a playbook importing itself is a hard error.

Verified 2026-08-02 (2.21.2, `playbook/role/__init__.py:232`): a **meta-dependency cycle
is a load-time hard error** — "A recursion loop was detected with the roles specified" —
so that case is a provable failure and can be ERROR severity, quoting Ansible's message.
The definitions walk (T-018) tolerates such cycles silently by design; this diagnostic is
where they get reported. Ansible caps no walk by depth anywhere — cycle detection only —
which is also the convention this codebase follows since the MAX_DEPTH removals.

Report once per cycle, anchored at the reference that closes it, with the full path in the
message. Reporting at every node in the cycle turns one problem into N.

Templated edges are the judgement call: a cycle that only exists through a glob candidate may
never happen at runtime. Follow literal edges only, so this can't warn on correct code.

## Done when

- [ ] a fixture with a 2-file and a 3-file cycle both report
- [ ] one diagnostic per cycle, message shows the whole path
- [ ] cycles reached only through templated candidates do not warn
- [ ] the real repo is checked; result recorded here either way
