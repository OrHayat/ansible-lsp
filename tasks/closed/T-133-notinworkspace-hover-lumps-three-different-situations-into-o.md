# T-133 — NotInWorkspace hover lumps three different situations into one vague message

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P3       | S    | T-118 | —          |

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

## What was measured before building it (2026-09-17, after T-083)

Three premises above did not survive:

- **"a 3-part FQCN provably isn't a builtin" is too wide.** `ansible.builtin.debug` is three
  parts and a builtin. The rule is the namespace, and after T-083 an `ansible.builtin.X` with an
  install either resolves, follows core's table, or is the `unknown-builtin-module` warning — it
  only reaches this hover through a rename to a collection that is not installed.
- **"collection exists → installed outside this workspace (<path>)" cannot happen.** A
  collection that has the module resolves, and resolved references take the provenance hover. A
  skipped name whose collection directory *is* found means the module is not in that copy.
- **`ansible.builtin.ufw` is a real name.** Core's table renames it to
  `community.general.ufw`; it ran on 2.21.3. The useful hover names the rename, which needed the
  hops kept on `Resolution` (`redirects`) — the same data T-083's internal-redirect box needs.

What reaches the arm, read from the resolver and then asserted through it: an unresolved module
chain (`resolve.rs`, end of `resolve_module`) and a `tasks_from:` whose role was not found. A
4-part name (`ns.coll.sub.module`, which Ansible runs from `plugins/modules/sub/` — measured) is
never extracted (`references.rs`, `dots <= 3`), so it has no hover at all rather than a wrong
one; not this ticket's.

| situation                                   | hover (after **Skipped** —)                                          |
| ------------------------------------------- | -------------------------------------------------------------------- |
| collection not installed, install found     | collection `ns.coll` is not installed here — fine if the machine that runs the playbook has it |
| collection not found, no install            | … is not in this workspace, and no Ansible install was found to look for it elsewhere |
| collection found, module absent             | `ns.coll` is installed at `<path>` but has no module `X`              |
| renamed by core's table, then one of above  | `ansible.builtin.ufw` is renamed to `community.general.ufw`, and …    |
| bare / `ansible.legacy.X`, install found    | `X` is not in ansible-core 2.21.3, nor in any `library` dir this workspace sees |
| bare / legacy / builtin, no install         | no Ansible install was found, so builtins and installed collections cannot be looked up |
| `tasks_from:`, role not found               | role `R` was not found, so its `tasks_from` file cannot be checked    |

## Done when

- [x] no case is described as possibly "a builtin" — asserted on every case
      (`a_reference_we_did_not_follow_says_why`)
- [x] a collection found without the module says where it was found and what it lacks (the
      reachable form of the original second box)
- [x] a collection found nowhere says that, without claiming the file is wrong
- [x] a name core's table renamed names the rename and where it stopped
      (`redirects`, pinned in `builtin_and_legacy_spellings_follow_cores_rename_table`)
- [x] no install says so; T-232's pre-startup control now asserts that wording
- [x] each case seen red by breaking its own branch
