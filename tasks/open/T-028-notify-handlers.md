# T-028 — `notify:` -> handler resolution

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P2       | M    | T-099 | —          |

## Problem

90 `notify:` references and 52 handler files, and none of it is checked or navigable.

This is a different **shape** of reference from everything built so far: it resolves a *name to
a task within a file*, not a path to a file. Worth its own ticket partly for that reason — it's
the first one that needs sub-file targets.

**Correction, measured on 2.21.2.** This ticket used to say a `notify:` naming a handler that
does not exist "is a silent no-op — Ansible does not error". That is wrong. It is **fatal**:
the play aborts with `The requested handler '...' was not found in either the main handlers
list nor in the listening handlers list`. But the check is gated on the notifying task
reporting `changed`, so it is silent on every converged run. The consequence the old wording
was reaching for survives and is worse than stated: the fault passes CI, `--syntax-check` and
`--check`, then aborts the first run that does real work. See
[`upstream/ansible-missing-handler.md`](../../upstream/ansible-missing-handler.md).

Rescoped: this ticket is now the **index and navigation**, which cannot produce a false
positive. The diagnostic moved to [[T-196]], because the set where it is provably safe is much
narrower than this ticket assumed — hence P2 here and P1 there.

## Approach

Collect handler names from `handlers/main.yml` in each role, `handlers:` blocks in plays, and
anything reachable from those via `include_tasks` inside a handler file. Two contributors the
original list missed, both measured:

- **`meta/main.yml` dependencies.** A role's dependencies' handlers are in the play. Measured:
  `gamma` depends on `beta`, the play lists only `gamma`, and `notify: beta handler` runs.
- **Dynamic `include_role`.** Contributes handlers too — and *when* it does depends on the
  include's position, which is [[T-197]].

Matching is not string equality against one spelling. From `search_handlers_by_notification`
(`plugins/strategy/__init__.py:504-510`) a role handler answers to **three** names:

```python
if notification in {
    handler.name,                              # bare, after templating
    handler.get_name(include_role_fqcn=False), # "role : name"
    handler.get_name(include_role_fqcn=True),  # "ns.coll.role : name"
}:
```

Measured: `notify: "beta : beta handler"` resolves. Indexing bare names only would leave every
role-qualified notify unnavigable, and would false-positive once [[T-196]] lands.

Three more rules, each of which breaks something if guessed instead of read:

- **No comma splitting.** `notify: "a, b"` is **one** handler name — measured fatal, not two
  lookups. `notify` is `isa='list'` with no split (`playbook/notifiable.py:10`), unlike `tags`
  (`playbook/taggable.py:53-54`). Splitting would silence a real fault.
- **A nameless handler contributes nothing.** `if not handler.name: continue`
  (`plugins/strategy/__init__.py:471`) — which is [[T-157]]'s block case. Skip them here and let
  that ticket report them.
- **Duplicate names.** `reversed` applies to the *blocks*, not to handlers within one, and the
  match loop breaks on its first hit. Measured: within one `handlers:` list the **first** wins;
  a play's `handlers:` beats a role's. Navigation must jump to the one that would actually run,
  or the hover contradicts the runtime.

Sub-file targets mean the resolver has to return a span inside the target file, not just a
path. `Resolution.targets` is `Vec<PathBuf>` today, so this is an added field — a small change
that T-024 wants anyway.

The AST half is done: `Task.notify`, `Task.listen` and `Block.notify` carry `HandlerRef { name,
span, templated }`, one entry per written name, with `templated` per name rather than per key.

## Done when

- [x] the AST carries `notify:`/`listen:` as named references with per-name spans
- [ ] `notify:` navigates to the handler task, positioned on it, not the file top
- [ ] all three spellings resolve, including `role : name`
- [ ] `notify: "a, b"` resolves as one name, asserted — the natural implementation splits it
- [ ] the index includes handlers from `meta/main.yml` dependencies
- [ ] duplicate names navigate to the one that would run, with both the within-block and
      across-block cases asserted; they resolve in opposite directions, so one test cannot
      cover both
- [ ] a nameless handler is absent from the index, cross-referenced to [[T-157]]
- [ ] templated `notify:` navigates nowhere and reports nothing here
- [ ] cross-role notify navigates but never warns — warning needs [[T-020]], since a role does
      not know its plays and the same notify is valid or fatal depending on them
- [ ] seen red before the fix, per rule 5
