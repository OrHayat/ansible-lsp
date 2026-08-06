# T-021 — `unused-file` / `unused-role` as faded hints

| Status | Priority | Size | Epic  | Depends on          |
| ------ | -------- | ---- | ----- | ------------------- |
| open   | P3       | M    | T-113 | T-020, T-018, T-013 |

## Problem

A grep approximation over `~/app/ansible` suggested 4 roles and ~74 task files are never
referenced. Dead Ansible is worse than dead code in a compiled language — nothing ever tells
you, and it accumulates until nobody dares delete anything.

## The honest limit, stated up front

**This tool cannot know whether something is used.** It sees one workspace. Jenkins invoking a
role by name, another repo installing from here, a Galaxy consumer, an operator running an
ad-hoc playbook — all invisible.

So the presentation has to match the confidence. Not a warning; a fade.

## Approach

`DiagnosticSeverity::HINT` + **`DiagnosticTag::UNNECESSARY`**. That tag is what greys the text
out — the same rendering VS Code gives an unused import in TypeScript or an unused variable in
Rust. It does not squiggle, does not put a marker in the gutter, and sorts to the bottom of the
Problems panel. Visually it reads *probably not needed*, not *this is broken*, which is exactly
the claim being made.

Only strong cases are shown:

| Situation                                                | Shown  |
| -------------------------------------------------------- | ------ |
| No inbound edges, no `galaxy_info`, not a playbook        | faded  |
| Declares `galaxy_info`, or the collection has `galaxy.yml` | never — published surface |
| Reachable only via a templated path                      | never — genuinely unknown |
| Top-level playbook                                       | never — entry points are invoked externally |

The `galaxy_info` exemption is what answers *"what if I publish the role — unused-role is
actually used now."* Publishing a role means adding `galaxy_info`, so the hint is silenced as a
side effect of the very thing that would make it wrong.

**It is a weaker signal than it first looked, and the numbers matter:**

- 23 of 74 roles have a `meta/main.yml`; 22 of those declare `galaxy_info`
- 2 of the 3 collections have `galaxy.yml` (`community/lvm`, `community/host` — `community/ceph`
  does not)
- of the 4 roles a first grep flagged as unreferenced, 3 have no `meta/` at all — but
  **`ceph-daemon` does, with `galaxy_info`**, so it would be exempted despite looking unused

So `galaxy_info` exempts ~30% of roles, not nearly all, and it does not cleanly partition
published from internal. It errs toward *not* flagging, which is the right direction for a
heuristic whose false positives are the expensive kind — but it is a filter, not a proof, and
the ticket should not pretend otherwise.

Plus `# noqa: unused-file` / `unused-role` for the cases the heuristics can't reach.

Blocked on T-018 as well as T-020: a role pulled in only as a `meta` dependency has no other
inbound edge, so shipping without it would fade roles that genuinely run.

## Done when

- [ ] unused files and roles render faded, not squiggled
- [ ] nothing with `galaxy_info` / `galaxy.yml` is ever flagged — including `ceph-daemon`,
      which looks unused but declares it
- [ ] `validate-expose.yml` is **not** flagged — it's reached via `validate-{{ _ap_op_type }}.yml`
- [ ] roles reached only through `meta/main.yml` dependencies are not flagged
- [ ] `scan` prints the same list as a report, for use outside an editor
- [ ] the hint's message says "no reference found in this workspace", never "unused"
