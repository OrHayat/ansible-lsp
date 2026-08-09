# T-147 — Validate role meta/main.yml against the RoleMetadata set

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | S    | T-106 | —          |

## Problem

`meta/main.yml` loads as `RoleMetadata` (`metadata.py:32-41`), whose legal set —
`Base` + `CollectionSearch` + `allow_duplicates dependencies galaxy_info argument_specs`,
27 keys — is already transcribed and oracle-verified in `keywords.rs`
(`KeyContext::RoleMetadata`, T-107). But nothing routes the file there: a top-level
mapping parses as `Ast::Other` and gets no diagnostics. So `when:` in `meta/main.yml` —
fatal at load, `RoleMetadata` mixes in no `Conditional` — is editor-silent, as is the
ansible-lint idiom `standalone:`. Meanwhile `become: true` there parses fine and does
nothing, which the rule must *not* flag.

## Approach

File-kind by path, not content: `FileContext` already knows `role_dir`; a file at
`<role>/meta/main.yml` whose top level is a mapping is the RoleMetadata context. Check its
top-level keys through the existing `legal_key(KeyContext::RoleMetadata, _)` and emit via
`attributes.rs` — always ERROR, there is no `INVALID_TASK_ATTRIBUTE_FAILED` escape for
this class. Values stay out of scope: `dependencies:` entries are RoleInclude-shaped and
`argument_specs:` has its own schema (T-149); this ticket is top-level keys only.

## Done when

- [ ] an unknown key in `meta/main.yml` gets an ERROR on the key span, message
      `'x' is not a valid attribute for a RoleMetadata`, with the one-edit suggestion
- [ ] every `Base` key (`become:`, `vars:`, …) stays silent there
- [ ] a file named `meta/main.yml` outside a role dir, and mapping-shaped files elsewhere,
      are untouched
- [ ] `# noqa: invalid-attribute` suppresses it
- [ ] zero findings on the demo roles and the real-tree sweep (74 roles in `~/app/ansible`)
