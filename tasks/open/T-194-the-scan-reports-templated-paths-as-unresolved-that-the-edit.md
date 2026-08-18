# T-194 — The scan reports templated paths as unresolved that the editor navigates

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P2       | S    | —          |

## Problem

## Approach

## Done when

- [ ]

## Problem

The scan and the editor give different answers about the same reference. In `~/app/ansible`:

```yaml
# roles/ad/tasks/renew.yml:39
include_tasks: "{{ ad_select_node_task }}"
```

`ad_select_node_task` is a plain literal in that role's own `defaults/main.yml`. Measured
through the public API on a fixture with the same shape:

```
resolve_in    -> Skipped,  targets=[]                         <- what bin/scan calls
resolve_with  -> Resolved, targets=[.../select-available-node.yml]   <- what the LSP calls
```

So the editor navigates it and the scan prints it under `TEMPLATED, MATCHES NOTHING`. Two
consumers, one question, two answers — the situation CLAUDE.md rule 3 exists to prevent.

`bin/scan.rs:102` calls `resolve_in`, which takes no literals. `main.rs:559` calls
`resolve_with_in` with `vars::known_literals_in`. Nothing is wrong with either; the scan simply
never asks the better question.

## Approach

Point the scan at the substituting entry point and give it the literals map it already has the
inputs for — it walks vars for `undefined_uses_in` through the same `ScanCache`.

**This cannot change the exit code.** `resolve_with` stamps `SkipReason::Templated` and turns
Missing into Skipped, so a substituted path is navigable but never warned about. The only thing
that changes is the honesty of the report.

**Measure the cost before believing it is free.** The `unknown-host` rule went from 140ms to
4.42s on exactly this kind of assumption (T-179), and was only saved by moving a cheap check
first. The var walk is cached, so this is probably small — but "probably" is what that episode
was made of.

Expected effect is modest and should be stated rather than oversold: of the 9 `TEMPLATED,
MATCHES NOTHING` lines on the corpus, this moves roughly one. Four are `{{ role_path }}`, whose
expansion is deliberately disabled pending T-068, and one is
`{{ ap_protocol }}/{{ _ap_operation_type_map[ap_operation] }}`, which is not a bare identifier
and cannot substitute until [[T-188]] lands.

T-135 covers collapsing the four resolve entry points into one. That is a refactor; this is the
behaviour change, and it does not need the refactor first.

## Done when

- [ ] the scan resolves a templated path whose variable has a known literal, asserted by a
      `scan_cli` test with the two-file shape above
- [ ] the exit code is unchanged for that fixture — a substituted path must never fail the gate
- [ ] a templated path with no known literal still lands in `TEMPLATED, MATCHES NOTHING`
- [ ] scan runtime on the corpus measured before and after and recorded here
- [ ] the corpus `TEMPLATED, MATCHES NOTHING` count recorded before and after; if it does not
      fall, say so rather than assuming the change worked
