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

Severity matrix, live-verified 2026-08-02:

- **meta cycle → ERROR, unconditionally.** `when:` on the dependency edges does NOT help:
  the recursion check runs at role load, before conditions exist. Proven: C1↔C2 with
  `when: false` on both edges still dies with "A recursion loop was detected…".
- **dynamic include cycle with a `when:` (or template) on ANY edge in the loop → INFO
  hint, never a warning.** Guarded mutual `include_role` is valid Ansible (proven: B→A
  `when: 'group_1' in group_names`, A→B `when: 'group_2' in group_names` runs clean,
  ok=3, for a host in one group). But guards being *different* doesn't make them
  *exclusive* (user counter-example: `when: 'group_a' in group_names` + `when:
  inventory_hostname == 'host0'` loops forever if host0 is in group_a — an inventory
  fact we don't read). So silence would hide real loops and warning would flag correct
  code (the legit `when: depth|int < 5` counter pattern). Middle tier: a faded hint that
  spells out the **loop condition** — the conjunction of every guard on the cycle:
  "repeats for any host where `'group_a' in group_names` and `inventory_hostname ==
  'host0'`". We state it; the user, who knows the inventory, evaluates it.
- **dynamic include cycle with no guard anywhere in the loop → WARNING.** Every host that
  enters recurses to death, but entry itself may be conditional, so not ERROR.
- **Follow-up, after T-062 lands ini inventory parsing:** evaluate the guard conjunction
  against the parsed inventory for the vocabulary it covers (group membership,
  hostnames). If a concrete host satisfies every guard, promote the hint to a WARNING
  naming that host ("loops forever for host0"). Conditions outside that vocabulary
  (facts, counters, registered vars) keep the hint. This makes T-022 a second consumer
  of T-062.

Also: the T-020 dependency is droppable for the reachable-from-open-file version — a
path-stack during the existing walk reports the closing edge with no reverse index.
Workspace-global detection (cycles in files nobody opens) stays with T-020.

## Done when

- [ ] a fixture with a 2-file and a 3-file cycle both report
- [ ] one diagnostic per cycle, message shows the whole path
- [ ] the three tiers pinned: meta cycle → ERROR even with `when:` on the edges;
      unguarded dynamic cycle → WARNING; guarded dynamic cycle → INFO hint whose message
      spells out the guard conjunction (the group_a/host0 counter-example verbatim)
- [ ] cycles reached only through templated candidates do not warn
- [ ] the real repo is checked; result recorded here either way
- [ ] (post-T-062) hint promoted to WARNING naming the host when the parsed inventory
      satisfies every guard
