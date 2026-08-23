# T-206 — include_vars searches its relative path in the wrong order, in two places

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-090 | —          |

## Symptom

A relative `include_vars:` resolves to a different file than the one Ansible loads. Found
while scoping T-184, whose canonical shape is the worst case:

```yaml
# roles/ad/tasks/main.yml
- name: Include role variables
  include_vars: main.yml      # Ansible loads roles/ad/vars/main.yml
```

We resolve that to **`roles/ad/tasks/main.yml`** — the task file including it. Measured on
ansible-core **2.21.3**, our side by a probe on `Resolver::resolve`:

| | Ansible loads | we resolve to |
| ---------------------- | ------------------------ | -------------------------- |
| `include_vars: main.yml` from `roles/ad/tasks/main.yml` | `roles/ad/vars/main.yml` | `roles/ad/tasks/main.yml` |

Four consumers read that target and each says something false:

- **go-to-definition** on the path opens the wrong file
- **the variable index** reads the wrong file, so the names the include really defines are
  absent — and here the file it reads is a *task list*, which defines nothing at all
- **hover** on any of those names answers from the wrong definition site, or not at all
- **`missing-file`** judges existence at the wrong path, so it can stay silent where Ansible
  fails and fire where Ansible succeeds

## Cause

The search order is wrong in three separate ways, and it is implemented **twice** — once for
navigation and diagnostics (`resolve.rs:494`), once for the variable index
(`vars.rs:1600 resolve_var_path`). The two lists are character-for-character the same order
and are maintained independently, so a fix to either leaves the other lying. Rule 3: one rule,
two callers.

Measured order, by placing a distinguishable copy at every candidate and deleting the winner
until the list ran out — so each position is a separate observation, not one lucky hit:

| # | Ansible (measured) | ours, both copies |
| - | -------------------- | ------------------- |
| 1 | `<role>/vars/`       | `<file_dir>/`       |
| 2 | `<file_dir>/vars/`   | `<file_dir>/vars/`  |
| 3 | `<file_dir>/`        | `<role>/vars/`      |
| 4 | `<project>/vars/`    | `<project>/`        |
| 5 | `<project>/`         | — |

1. **`<role>/vars/` must be first; ours is third.** This is the one that produces the symptom
   above, because `<file_dir>` *is* `<role>/tasks` and a role's task file is usually
   `main.yml` — so the lookup finds itself.
2. **`<file_dir>/vars/` comes before `<file_dir>`; ours is reversed.** Measured separately
   **outside** any role, so this half is not a role bug — it is wrong for every relative
   `include_vars:` in the workspace.
3. **`<project>/vars/` is missing from our list entirely**, so a project-level `vars/` file is
   reported missing.

The control that matters for (1): with only the `<file_dir>` copy present, Ansible loads it.
So `<file_dir>` really is on the search path and this is an ordering fault, not a
"`<role>/vars/` only" one — the probe could produce either answer and did produce both.

Upstream is `_find_needle('vars', source)` → `path_dwim_relative_stack`, which tries
`<path>/vars/<source>` then `<path>/<source>` for each path on the stack. That shape explains
every row: the stack is role, then file dir, then project, and each entry is tried `vars/`
first. Ours interleaves them differently and drops one.

## Fix

One ordered candidate list, built in one place, consumed by both callers — the duplication is
half the bug and leaving it means the next fix lands in one copy again.

Note that `include_vars.rs` deliberately refuses this lookup: its header says the `file:`
search order is out of scope and a relative `file:` returns `Outcome::NeedsNeedle` rather than
guessing. The two lists above are that same guess, made twice, in modules that did not say
they were making it. Whether the shared implementation belongs in `include_vars.rs` — closing
`NeedsNeedle` — or in `workspace.rs` beside the other search-path builders is the design call
this ticket has to make.

**Not measured, and not assumed either:** whether `vars_files` and the task kinds share this
defect. They build candidates by different functions (`vars_files_candidates`, `task_bases`)
against separately measured semantics, so they are out of scope here — but nobody has run the
same probe against them, and that is a gap, not a clean bill of health.

## Done when

- [ ] one candidate-order implementation; `resolve.rs:494` and `vars.rs:1600` both call it and
      neither carries its own list
- [ ] all five positions asserted in order, each by deleting the winner and re-resolving, so a
      single re-ordering cannot pass by accident
- [ ] `include_vars: main.yml` from `<role>/tasks/main.yml` resolves to `<role>/vars/main.yml`,
      with `<role>/tasks/main.yml` present — the case that has to fail today
- [ ] the non-role half asserted without a role in the tree, so the `<file_dir>/vars/` before
      `<file_dir>` fix is pinned independently of the role fix
- [ ] `<project>/vars/` resolves
- [ ] a test per consumer, not per rule: go-to-definition target, the indexed `VarSource::IncludeVars`
      definitions, hover's definition site, and `missing-file` — each asserted separately
- [ ] seen red before the fix, for the right reason
- [ ] T-184 unblocked: its rule can key on the resolved target and fire on the shape it names

**Blocks:** [[T-184]], whose approach assumes the resolved target is already correct.
