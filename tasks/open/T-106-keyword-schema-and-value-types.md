# T-106 — Keyword schema and value types

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | epic | P1       | L    | —          |

## Problem

Ansible's playbook schema is closed and knowable: every legal keyword, in every context, with
its type and sometimes its legal values, is declared as a `FieldAttribute` and enforced by
`FieldAttributeBase._validate_attributes` (`playbook/base.py:211-220`). Violations are
`AnsibleParserError` at load — the user's play does not run at all.

We already extracted a version of this once (T-045) and built the AST to hang it on (T-044),
and then used both for resolution only. This epic is those two finally being spent on what
they were built for.

The reason it is one epic and not five tickets is that all five children read the **same six
tables** — Play, Block, Task, Handler, RoleInclude, RoleMetadata. Transcribing those tables
is most of the work; legal-set, type, enum, placement and `module_defaults` are five columns
and one structural pass over the result. Done separately, the tables get transcribed five
times and drift five ways.

One deliberate exclusion: `keyword_desc.yml` is **not** the schema. It omits `listen`,
`validate_argspec` and `local_action`, and documents prose with no corresponding key. It is a
docs artefact. The `FieldAttribute` declarations are the source of truth, and the transcription
should keep the mixin composition visible so a future ansible-core can be diffed against it.

Most of this is redundant with `ansible-playbook --syntax-check`, which does run the full
load chain (`executor/playbook_executor.py:110-156`). The value is not new coverage but
timing and locality: per keystroke, on the offending line, without an entrypoint. Two things
here do beat `--syntax-check` outright — `order:` and `strategy:` are validated only at run
time (T-109), and keys inside `apply:` are loaded at include-expansion time (T-101, in the
sibling epic).

## Children

- [ ] T-088 — Unknown play keyword: Ansible refuses the play, the editor says nothing
- [x] T-044 — Semantic AST (Play / Block / Task / Role)
- [x] T-045 — Keyword schema from Ansible's FieldAttributes
- [ ] T-107 — Per-class keyword sets from FieldAttribute
- [ ] T-108 — Keyword value types: isa coercion and listof
- [ ] T-109 — Keyword value enums
- [ ] T-110 — Placement and mutual-exclusion rules
- [ ] T-111 — module_defaults: shape, the 3-segment rule, and action groups

## Done when

- [ ] every child is closed or rejected
- [ ] the six tables live in one module with the ansible-core version they were read from
- [ ] the corpus scan over `~/app/ansible` reports zero false positives — a schema rule that
      fires on a working repo is wrong by construction
