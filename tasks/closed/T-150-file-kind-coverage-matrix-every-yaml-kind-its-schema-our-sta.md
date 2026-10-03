# T-150 — File-kind coverage matrix: every YAML kind, its schema, our status

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | task | P2       | S    | —          |

## Problem

`Ast` classifies by *content* (`Playbook | Tasks | Other`), but an Ansible project has many
more file kinds than that, most distinguishable only by *path*, each with its own schema
and its own validator inside ansible-core. Coverage decisions have been made one file at a
time (T-147..T-149 came out of one conversation); nothing records the full universe, so
"did we miss a kind?" has no checkable answer. T-107's audit showed what that costs.

The matrix. **Read** = we consume the file's content today; **Validated** = we check its
keys against a schema. Both columns are verified against the tree at the cite given, and the
core column against the installed **ansible-core 2.21.2**. Verified 2026-10-03.

| Kind | Schema owner in core (2.21.2) | Read | Validated | Ticket |
| --- | --- | --- | --- | --- |
| playbook | `playbook/base.py` `FieldAttribute` MRO | yes — `ast.rs:19` `Ast::Playbook` | yes — `attributes.rs`, `keywords.rs` | ~~T-107~~ |
| tasks / handlers files | same + include restrictions | yes — `ast.rs:24` `Ast::Tasks` | yes | ~~T-107~~ |
| role `meta/main.yml` | `playbook/role/metadata.py:32-41` — `RoleMetadata`, 4 fields | deps only — `vars.rs:774` | top-level keys — `attributes.rs:75-107`, `KeyContext::RoleMetadata` | ~~T-147~~, T-209 (values), T-210 (the 22 inert keys) |
| role `meta/argument_specs.yml` | `arg_spec.py` validator | **no** — the name appears only as a keyword (`keywords.rs:136`) and in a test asserting it is *not* `RoleMetadata` (`main.rs:8747`) | no | T-041 (call sites), T-149 (the file) |
| collection `meta/runtime.yml` | collection loader (unaudited) | routing only — `cache.rs:744-779` | no | T-148 |
| collection `galaxy.yml` | `galaxy/data/collections_galaxy_meta.yml` — machine-readable, per-key `required`/`type` | **no** — zero references in `crates/` | no | T-151 |
| `requirements.yml` | `cli/galaxy.py` (list, or roles/collections dict) | **no** — zero references in `crates/` | no | T-151 |
| **YAML inventory** | `plugins/inventory/yaml.py` (`all`/`hosts`/`vars`/`children`) | **yes** — hosts `inventory.rs:504`, vars `inventory.rs:589` | no — the `all`/`hosts`/`children` shape is unchecked | T-152 |
| **INI inventory**, bare group_vars | `plugins/inventory/ini.py` | **yes** — hosts `inventory.rs:540`, vars `inventory.rs:640` | n/a | ~~T-062~~ |
| **TOML inventory** | `plugins/inventory/toml.py` | **yes** — hosts `inventory.rs:569`, vars `inventory.rs:355` | n/a | ~~T-062~~ |
| `group_vars/` `host_vars/` (.yml) | vars files — keys are user-chosen names | yes (indexed) | n/a | — |
| `defaults/` `vars/` files | same | yes (indexed) | n/a | ~~T-239~~, ~~T-243~~ extended which filenames count |
| playbook `.meta` files | `playbook/play.py:73` `validate_argspec`, post-validated at `396-424` | **no** — `validate_argspec` is a keyword name only (`keywords.rs:92`) | no | T-153, and T-240 now covers the same ground |
| `ansible.cfg` | `config/base.yml` — 220 settings in 2.21.2 | yes | partly — 19 of 220 read; all 220 classified | ~~T-098~~, ~~T-144~~ |
| vault-encrypted files | `$ANSIBLE_VAULT` header | **no** — zero references in `crates/`; parses as one scalar, `Ast::Other`. A vaulted vars source makes `var-undefined` fire falsely (probed) | n/a | T-037 (~~T-154~~ was a duplicate) |
| `files/`, `templates/` (Jinja) | not YAML | n/a | n/a | out of scope here |

### What this pass corrected

Three rows were wrong, all in the same direction — the table understated us, because T-062
landed after it was written and nobody came back:

- **YAML inventory** read `no` → **yes**. `inventory.rs` has parsed hosts and vars since
  T-062. T-152 still owns the row, but its remaining scope is *validating* the
  `all`/`hosts`/`children` shape, not reading it.
- **INI inventory** read `no` → **yes**, same commit, and its ticket T-062 is closed, so the
  row is struck rather than owned.
- **TOML inventory was missing from the matrix entirely.** `plugins/inventory/toml.py` is a
  core inventory plugin and `inventory.rs` reads it (`toml_hosts`, `toml_vars`). A kind
  absent from the register is the precise failure this ticket exists to make impossible, and
  it was absent.

One cite was stale: playbook `.meta` was cited at `play.py:452-475`; in 2.21.2 the attribute
is `play.py:73` and its post-validation `396-424`.

Non-core files (`.ansible-lint`, `ansible-navigator.yml`, `execution-environment.yml`)
are deliberately absent: we mirror ansible-core, not its ecosystem.

## How to add a row

Any newly-discovered file kind lands here **first**, before it gets a ticket:

1. Add the row with Read/Validated set to what is true *today*, each with a cite — a file and
   line in `crates/`, not a recollection. "No" needs a cite too: the evidence for a `no` is a
   search that came back empty, and saying which search makes it re-runnable.
2. Name the core-side schema owner with a path and line from the **installed** core, and
   record the version audited. Cites rot — this pass found one three releases stale.
3. Give it a ticket, a struck ticket, or a recorded won't-do. A row with an empty Ticket cell
   is the gap this register exists to make visible, so it never ships empty.

The register is the index, not the work. When a kind's ticket closes, strike it here in the
same commit; a row pointing at a closed ticket reads as open work and sent this pass hunting
T-062 twice.

## Approach

This ticket is the register, not the work: verify each unverified row the T-107 way (read
the consuming source path whole, record cites), correct the table where wrong, and keep it
current as kinds get tickets. A row may resolve to "won't do" — that's a recorded outcome,
not a gap.

## Done when

- [x] every row's Read/Validated status is source-verified, with cites in this file
      — 16 rows, each cited to a file and line in `crates/` or to an empty search; the core
      column re-verified against the installed 2.21.2
- [x] every row has a ticket, a struck ticket, or a recorded won't-do rationale
- [x] a "how to add a row" note: any newly-discovered kind lands here first
