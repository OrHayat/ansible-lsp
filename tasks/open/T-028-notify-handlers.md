# T-028 — `notify:` -> handler resolution

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P3       | M    | —          |

## Problem

90 `notify:` references and 52 handler files, and none of it is checked or navigable.

This is a different **shape** of reference from everything built so far: it resolves a *name to
a task within a file*, not a path to a file. Worth its own ticket partly for that reason — it's
the first one that needs sub-file targets.

The reason it's worth doing at all: **a `notify:` naming a handler that doesn't exist is a
silent no-op.** Ansible does not error. The handler simply never runs, so the service never
restarts, and you find out when the config change didn't take effect — possibly much later,
possibly in production. That's the highest-consequence, lowest-visibility failure mode in this
whole backlog.

## Approach

Collect handler names from `handlers/main.yml` in each role, `handlers:` blocks in plays, and
anything reachable from those via `include_tasks` inside a handler file. Match `notify:` values
against that set. Both string and list forms.

The matching rules are where the care is needed — get these wrong and it warns on correct code:

- notify can name a **`listen:` topic** rather than a handler name, and several handlers may
  listen to one topic. A topic with no listeners is the real bug; a topic *is* a valid target.
- names are matched **literally, after templating**, so any templated `notify:` has to be
  skipped entirely — no glob equivalent exists for names.
- scope: a role's handlers are visible to that role and to plays that include it. Cross-role
  notify works in practice but only if the other role has run. Resolving it is fine;
  *warning* about it is not — "is the other role in this play" isn't statically knowable.

Given that, split it: navigation for everything, and a `missing-handler` warning only for the
unambiguous case — a literal name, in a role that has handlers, matching neither a handler name
nor a `listen:` topic anywhere in the workspace.

Sub-file targets mean the resolver has to return a span inside the target file, not just a
path. That's a small change to `Resolution` that T-024 would want anyway.

## Done when

- [ ] `notify:` navigates to the handler task, positioned on it, not the file top
- [ ] `listen:` topics resolve, and a topic with no listeners warns
- [ ] templated `notify:` never warns
- [ ] cross-role notify navigates but never warns
- [ ] corpus gate: measure first — if `missing-handler` fires on the real repo, each one is
      either a real silent no-op or a resolver bug, and which is which goes in this ticket
