# T-061 — `undeclared-var`: surface the playbook's required `-e` inputs

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P2       | S    | T-112 | T-062      |

Split out of T-033 (its LOUD class) — the corpus research lives there.

## Problem

An **unguarded** `when:` variable defined nowhere makes Ansible die mid-run with
`'x' is undefined`. The corpus has 9 such variables over 55 uses (`zfs_role`,
`s3_user_action`, `ad_join_node`, …) — and they are mostly not bugs: they're the
playbook's **required `-e` inputs**. Nothing documents that contract, so forgetting one
fails against a live cluster.

T-051's `var-undefined` already warns on this class *inside playbooks*; what's left is the
contract angle: uses in role/tasks files (where per-file warning is undecidable — T-059
covers call sites), aggregated and presented as documentation rather than as defects.

## Approach

INFORMATION severity, not warning — these are inputs, not errors. The main deliverable is
the `scan` section: one aggregated list per workspace, "required from `-e` or inventory:
`zfs_role` (22 uses), …", so the contract is documentable outside an editor. In-editor,
an INFO hint at each use, `# noqa: undeclared-var` to silence.

Distinct from T-060: no near-miss filter here — unguarded means the run *stops*, so even a
legitimate `-e` input is worth surfacing once.

## Done when

- [ ] the corpus's LOUD variables are reported as INFORMATION, and no others
- [ ] `scan` prints the aggregated `-e` contract list with use counts
- [ ] `# noqa: undeclared-var` works
- [ ] overlap with T-051's `var-undefined` resolved: a use flagged by one is not
      double-reported by the other
