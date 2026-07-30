# T-024 — Execution tree as a TreeView

| Status | Priority | Size | Depends on   |
| ------ | -------- | ---- | ------------ |
| open   | P3       | L    | replaces T-011 |

## Problem

The original ask, still unmet: *"no way to give a playbook an execution context to see what
will be called."* Reading `site.yml` tells you nothing about what actually runs — plays ->
roles -> `tasks/main.yml` -> nested includes -> `meta` dependencies is a dozen files of manual
tracing.

T-011 tried this through LSP call hierarchy and was rejected: `prepareCallHierarchy` had no
symbol to anchor to, so it answered every cursor position with the whole file's calls. Read
that ticket before starting — the failure was in the framing, not the code.

## Approach

An explicit `Ansible: Show Execution Tree` command feeding a sidebar TreeView, **rooted at a
playbook the user picks**. Making the root explicit is the entire fix: the scope is stated
rather than implied, so the view cannot misrepresent what it covers.

`graph.rs` expands using the existing resolver. Visited-set of resolved absolute paths for
cycle detection.

What VS Code was doing for free now has to be written (~80 lines): traversal, lazy expansion,
cycle handling. That's the price of the honest version.

**Node labels have to be careful, or this repeats T-011's real sin — overstating certainty:**

| Label           | Means                                                                 |
| --------------- | --------------------------------------------------------------------- |
| conditional     | task has `when:` — may not run                                        |
| repeated        | `loop:`/`with_*` — runs N times, N unknown                            |
| dynamic         | templated target; show glob candidates as children, not one guess     |
| cycle           | already expanded above                                                |
| **pushed down** | `when:` on an `import_playbook` — see below                           |

*Pushed down* is the one that must not be collapsed into *conditional*. Ansible copies a
`when:` on an import onto every task in every imported play; it is not a gate on the subtree.
Rendering it as "conditional" would be a lie the tree tells about control flow.

Collapse repeated call sites: 20 × `access-point` becomes one node with a count, or the tree is
unreadable on `site.yml`.

VS Code only — a TreeView has no LSP equivalent, so this does not carry to Neovim (T-026). The
`graph.rs` expansion is reusable there via a custom request whenever that matters.

## Done when

- [ ] the command asks which playbook, and the view states its root
- [ ] `site.yml` expands through `daos` + `daos-cluster` without hanging
- [ ] output diffs cleanly against `ansible-playbook --list-tasks site.yml` for the static
      subset (that oracle expands `import_*` but not `include_*`, so the include path is only
      partially validated)
- [ ] every label above is exercised by the demo playbook
- [ ] `when:` on an import is labelled *pushed down*, never *conditional*
