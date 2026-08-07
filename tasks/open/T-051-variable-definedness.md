# T-051 — Variable definedness diagnostic

| Status | Priority | Size | Epic  | Depends on   |
| ------ | -------- | ---- | ----- | ------------ |
| open   | P1       | M    | T-112 | T-048, T-049 |

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

  Those two rules together cover almost all of `INTERNAL_STATIC_VARS`
  (`constants.py:79-117`) — the `ansible_` prefix absorbs `ansible_playbook_python`,
  `ansible_config_file`, `ansible_play_name`, `ansible_role_name(s)`, `ansible_play_hosts`,
  `ansible_play_batch`, `ansible_version`, `ansible_limit`, `ansible_run_tags` and the rest.
  Exactly **three** reserved names fall through both rules and must be added to
  `condition::MAGIC` or they become false "undefined" warnings on ordinary lines:

  ```
  inventory_file      inventory_dir is in MAGIC, its twin is not
  role_uuid
  role_names          role_name is in MAGIC, the plural is not
  ```

  For definedness the prefix rule is enough and no *value* is needed. Recording one anyway,
  because it was asked twice and lived only in a session log: **`ansible_playbook_python`
  is derivable.** `ansible --version` prints

  ```
  python version = 3.12.3 (main, ...) [GCC 13.3.0] (/path/to/bin/python)
  ```

  and the interpreter is the last parenthesised group. `install.rs:155-181` already runs
  that command and splits each line on `=`, so it is one more match arm plus a
  `rsplit_once('(')`. On the filesystem fast path (no subprocess, T-084) it would have to be
  inferred from `package_dir` — `.../lib/python3.12/site-packages/ansible` → `<prefix>/bin/python`,
  or `<prefix>/Scripts/python.exe` on Windows — which is convention rather than fact. And it
  is the install *we* detect, not necessarily the one invoked, so it carries the same
  launch-time uncertainty as `-e` and `$CWD`. Nothing needs this today; it is written down so
  the next person does not re-derive it.
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
`var-undefined`, noqa-suppressible, also reported by `scan`.

**Gate correction (same day):** the "zero corpus hits" first reported was an artifact — the
scan run had panicked mid-corpus (byte-boundary slice in `softened()` on a block-scalar
file; spans are value-relative and shift off char boundaries; fixed and pinned). The real
count is **656**. Grep-verified diagnosis (correcting an earlier guess about role
defaults): the names are mostly defined in the corpus's project-root `group_vars/all.yml`
— an **inventory-adjacent** file our walk never reads, i.e. the T-062 gap. Per this
ticket's own rule, the rule doesn't ship at that count. Interim fix is **T-065**: require
"defined nowhere in the whole workspace"; T-062 is the real repair for this class, after
which the reachability bar gets re-measured. Also noted: the walk's
`MAX_DEPTH` truncation can hide real definitions; when that matters, suppress
`var-undefined` on truncated walks rather than report from partial knowledge.

## Done when

- [x] a use guarded more broadly than its definitions cover is flagged, naming the gap
      (`var-uncovered-when`, `guard.rs`)
- [x] a genuinely-undefined variable (no reachable definition at all) is flagged, with a
      message that concedes the opaque sources (`var-undefined`)
- [x] zero warnings on magic vars, `ansible_*`, loop vars, or anything with a reachable def
      — each pinned by a test
- [x] role/task files that legitimately receive vars from a caller are not false-flagged
      (playbooks only, pinned)
- [ ] corpus gate: ~~zero hits~~ **656 real hits** once the panicked run was fixed — the
      workspace-wide-absence tightening above must land and re-gate before this ticks
- [x] supersedes T-033 (the `when:`-only version), or explicitly narrows to it — resolved
      by splitting T-033: its playbook-level unguarded class is absorbed here; the
      near-miss rule (T-060), `-e` contract (T-061), and inventory indexing (T-062) are
      their own tickets, with caller-level checking opened as T-059
