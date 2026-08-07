# T-133 — NotInWorkspace hover lumps three different situations into one vague message

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P3       | S    | T-118 | —          |

## Problem

Hovering community.general.include_tasks says 'Skipped — not in this workspace (a builtin, or installed outside it)'. One canned sentence covers three distinct cases (builtin, installed outside the workspace, not installed at all) and for a 3-part FQCN the 'a builtin' half is provably wrong. The server often knows which case it is and could say so.

## Approach

The message is one arm in `reference_hover`'s skip handling
(`crates/ansible-lsp/src/main.rs:1524-1526`). At that point the reference's name shape and
the detected install (`AnsibleInstall::detect()`) are both at hand, so the arm can split:

- `ansible.builtin.X` / bare name with no local hit → "a builtin — installed outside this
  workspace" (and with an install detected, whether it's actually there).
- `ns.coll.X` where the collection exists under the detected install's
  `ansible_collections/` → "installed outside this workspace (<path>)".
- `ns.coll.X` found nowhere → "collection `ns.coll` is not installed here — fine if the
  target machine has it". Never "a builtin": a 3-part FQCN provably isn't one.

Overlaps with what T-039 (requirements.yml cross-check) and T-118's routing work need
anyway; whoever gets there first should share the "is this collection installed, where"
lookup.

## Done when

- [ ] a 3-part FQCN is never described as possibly "a builtin"
- [ ] when the collection is found under the detected install, the hover says so
- [ ] when it is found nowhere, the hover says that, without claiming the file is wrong
