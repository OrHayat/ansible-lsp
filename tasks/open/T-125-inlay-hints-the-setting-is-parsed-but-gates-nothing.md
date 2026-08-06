# T-125 — Inlay hints: the setting is parsed but gates nothing

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P2       | S    | T-124 | —          |

## Symptom

`ansibleLsp.inlayHints.enabled` appears in the VS Code settings, is read on
`didChangeConfiguration` into `Settings::hints`, and **does nothing**. Toggling it changes no
behaviour. There is no inlay-hint provider in `ServerCapabilities` and no handler —
`grep -rn "inlay_hint\|InlayHint" crates/ client/src/` returns nothing.

Two tickets still describe hints as shipped: T-032's done-when refers to them, and T-025
plans settings around them. `scripts/inlay-hints.py` is still in the tree and named for a
feature that is not there.

## Cause

T-078 moved the `when:` explanation from an inlay hint to hover, because the hint and the
module-provenance hover were fighting over the same token. The provider was removed; the
setting, the script name and the two tickets' prose were not.

Nobody has been misled *yet* only because the setting defaults to off and the feature it
names never existed to be missed.

## Fix

Decide, then make the tree say the same thing either way:

- **Remove** — drop the setting, rename or delete `scripts/inlay-hints.py`, and correct the
  prose in T-032 and T-025. Smallest, and honest.
- **Restore** — implement `textDocument/inlayHint` for the `when:` explanation, with the
  hover/hint conflict resolved as T-078 required.

Recommend removing. Hover already carries the explanation, T-078 chose it deliberately after
finding the conflict, and a setting that gates nothing is worse than no setting.

## Done when

- [ ] the setting either works or is gone
- [ ] `scripts/inlay-hints.py` matches whatever was decided
- [ ] T-032 and T-025 no longer describe inlay hints as shipped
- [ ] `grep -rn inlay` across the repo returns nothing surprising
