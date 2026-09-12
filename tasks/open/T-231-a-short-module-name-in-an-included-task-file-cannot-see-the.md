# T-231 — A short module name in an included task file cannot see the caller's collections: list

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-118 | —          |

## Problem

[[T-042]] taught the resolver the `collections:` search list, but only where the list and the
short name are in the same file — or where the list is the role's own `meta/main.yml`, which
is on disk and reachable from any file inside the role. The one shape left out is the one
where the list belongs to a **caller**:

```yaml
# p1.yml — the lookup rule lives here
- hosts: all
  collections: [ns.a]
  tasks:
    - include_tasks: inc.yml
```
```yaml
# inc.yml — the short name lives here
- shared_name:
```

Measured on 2.21.2 (`scratchpad/t042_cross_file_probe.sh`): this runs, and
`shared_name` resolves to `ns.a.shared_name`. Drop the `collections:` line and the play dies.
So the list crosses the file boundary — unlike into a role, which resets the search
([[T-042]] row J).

We resolve one file at a time. Opening `inc.yml` there is no list in sight, so the name is
`Skipped`/`NotInWorkspace`: no hover, no go-to-definition, no verdict. Silent, not wrong.

### It is not one missing answer, it is possibly several

`scratchpad/t042_two_callers_probe.sh` — one `inc.yml`, two playbooks, different lists, with
both collections shipping a module of that name:

| Run | `shared_name:` in `inc.yml` resolved to |
| --- | --------------------------------------- |
| `p1.yml` (`collections: [ns.a]`) | `ns.a.shared_name` |
| `p2.yml` (`collections: [ns.b]`) | `ns.b.shared_name` |

And where only one caller's list carries the name, the same file is simultaneously fine and
fatal:

| Run | `only_a:` |
| --- | --------- |
| `p1.yml` (`ns.a` ships it) | `ns.a.only_a` |
| `p2.yml` (`ns.b` does not) | `couldn't resolve module/action 'only_a'` — the play dies |

One line, two correct answers. This is why it is not a matter of fetching the list: an
editor showing `inc.yml` alone has no single right answer, and picking one caller's would be
a confident wrong hover on the other's.

Same shape as [[T-068]]: a fact about the **invocation**, not about the file.

## Approach

[[T-020]] already built the half that is hard. `ReverseIndex::inbound(target) -> &[Edge]`
answers "who includes this file", from the same `(Reference, Resolution)` pairs the scan
already computes — one map insert per edge, no extra read or parse. Since [[T-042]],
`Reference::collections` carries the list in scope at the include site, so the list is
already sitting on the reference that becomes the edge.

1. `Edge` gains the caller's list; `edges_of` copies it off the reference.
2. `resolve::collections_in_scope` gains a fourth branch, after the role-meta one: ask the
   index for this file's inbound edges.
3. Three cases:

| Callers | Verdict |
| ------- | ------- |
| one, or several that agree | resolve under that list — hover, go-to-definition and diagnostics, same as a list written in the file |
| several that **disagree** | navigate, never warn: offer every caller's target, emit no diagnostic |
| none found | stay silent, as today |

The middle row is the existing `SkipReason::Templated` contract — resolve enough to
navigate, marked so no rule can warn on it — and not a fudge: a file with two callers
genuinely has two answers, so offering both is the truthful move where picking one is not.

**The real cost is ordering.** Resolution currently *feeds* the index; this makes resolution
*read* it. Whatever shape that takes has to survive an edit to a caller — changing
`collections:` in `p1.yml` changes the right answer in `inc.yml`, a file that did not change.

### Why "no caller found" is silence and not a warning

Measured, `scratchpad/t042_orphan_probe.sh`:

| Case | 2.21.2 |
| ---- | ------ |
| nothing includes `inc.yml` | never read — the play ran clean |
| `ansible-playbook inc.yml` directly | `'only_a' is not a valid attribute for a Play` — a different error; it is a task list, not a playbook |
| included via `include_tasks: "{{ f }}"` | runs, resolves under the play's list |
| included from a playbook **outside** the tree | runs, the outside playbook's list applies |

Row 1: an orphan task file never executes, so there is no run to be wrong about — silence is
not a missed warning, there is nothing there. Row 4: "no caller in the workspace" is not "no
caller", so warning on it would fire on working files every time the real caller lives
outside the tree. That is the false positive this project exists to avoid.

Row 3 is good news for the plan rather than a complication: [`reverse`] records an edge for
**every** candidate a templated reference reaches, so `include_tasks: "{{ f }}"` still links
the caller. It widens the "callers agree" case instead of narrowing it.

## Done when

- [ ] a short module name in a task file resolves under its caller's `collections:` list when
      the workspace holds exactly one caller, or several that agree
- [ ] a file with callers whose lists **disagree** navigates to every caller's target and
      produces no diagnostic, pinned by the two-caller fixture above
- [ ] a file with no caller in the workspace stays silent, with a test naming the orphan and
      outside-caller rows as the reason
- [ ] editing a caller's `collections:` list updates the included file's answers, pinned by a
      test that edits the caller and re-resolves the target
- [ ] a short *role* name inside an included file gets the same treatment as a module, or a
      measurement says it does not need it
