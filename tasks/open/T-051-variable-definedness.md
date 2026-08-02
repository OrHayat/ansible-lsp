# T-051 — Variable definedness diagnostic

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P1       | M    | T-048, T-049 |

## Problem

The daily Ansible bug: a `{{ variable }}` (or a `when:` name) is misspelled or never set, so
at runtime it's silently empty — no error, wrong behaviour. The variable model
([`crate::vars`]) now knows both **uses** (T-049) and **definitions** across files (T-048), so
a use with no reachable definition can be flagged.

This is the payoff of the whole variable model — and the most dangerous feature here. A false
"undefined" is worse than none, because it trains people to ignore the squiggle. It must be
conservative to the point of near-silence.

## Approach

For each `VarUse` whose name has **zero** entries in `vars::definitions`, warn — but only after
ruling out everything that could legitimately supply it:

- Ansible **magic variables** (`inventory_hostname`, `groups`, `hostvars`, `role_path`,
  `playbook_dir`, `item`, …) — reuse `condition::MAGIC`.
- Anything starting `ansible_` (facts).
- A `register`/`set_fact` anywhere reachable (already in the def index, so covered).
- **Caller-injected** vars: a role/included file can receive vars from whoever includes it, and
  that caller isn't visible from the file alone. If the file is an include target (not a
  top-level playbook), suppress — or only diagnose playbooks, not role/task files.

Because inventory group_vars/host_vars and `-e` are not indexed, the message must concede them:
"used but not defined in any file reachable from here — may still come from inventory, facts,
or extra-vars."

## Traps / limits

- Loops define `item`/`loop_var` — honour `loop_control: loop_var`.
- `vars_prompt`, `include_vars` (until T-053) define names we may not yet index — err toward
  silence while those sources are incomplete.
- Suppressible via `# noqa`, like every other diagnostic.

## Progress

The **condition-aware coverage** half landed (commit `9b4b016`): `crate::guard` does
propositional implication over `when:` conditions, and `var-uncovered-when` warns when a use
runs under a broader condition than any in-effect definition covers (e.g. used for
`web01 or web02`, registered only on `web01`). Conservative — only with a definition present,
only within the use's own condition vocabulary.

The **never-defined-anywhere base case** landed 2026-08-02: `vars::undefined_uses`,
playbooks only (a tasks/role file can receive vars from any caller), exempting magic vars,
`ansible_*`, `loop_var`/`vars_prompt`/`{% set %}` declarations, any reachable definition
(even a later one — ordering stays the uncovered-`when` check's business), and uses whose
own expression or guard handles undefinedness (`default(…)`, `is defined`). Diagnostic
`var-undefined`, noqa-suppressible, also reported by `scan`. Gate: **zero** hits on
`~/app/ansible` (731 files); the demo's three hits are deliberate showcases.

## Done when

- [x] a use guarded more broadly than its definitions cover is flagged, naming the gap
      (`var-uncovered-when`, `guard.rs`)
- [x] a genuinely-undefined variable (no reachable definition at all) is flagged, with a
      message that concedes the opaque sources (`var-undefined`)
- [x] zero warnings on magic vars, `ansible_*`, loop vars, or anything with a reachable def
      — each pinned by a test
- [x] role/task files that legitimately receive vars from a caller are not false-flagged
      (playbooks only, pinned)
- [x] corpus gate: zero hits on `~/app/ansible`; the demo's three are deliberate and
      labelled in their comments
- [ ] supersedes T-033 (the `when:`-only version), or explicitly narrows to it — the base
      case covers `when:` names too (uses() walks them, with the definedness softening);
      T-033's fate needs its ticket read before closing
