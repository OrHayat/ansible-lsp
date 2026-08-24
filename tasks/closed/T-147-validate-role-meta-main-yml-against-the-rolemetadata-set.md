# T-147 — Validate role meta/main.yml against the RoleMetadata set

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P2       | S    | T-106 | —          |

## Problem

`meta/main.yml` loads as `RoleMetadata` (`metadata.py:32-41`), whose legal set —
`Base` + `CollectionSearch` + `allow_duplicates dependencies galaxy_info argument_specs`,
27 keys — is already transcribed and oracle-verified in `keywords.rs`
(`KeyContext::RoleMetadata`, T-107). But nothing routes the file there: a top-level
mapping parses as `Ast::Other` and gets no diagnostics. So `when:` in `meta/main.yml` —
fatal at load, `RoleMetadata` mixes in no `Conditional` — is editor-silent, as is the
ansible-lint idiom `standalone:`. Meanwhile `become: true` there parses fine and does
nothing, which the rule must *not* flag.

### Measured, on ansible-core 2.21.3

A one-task role with `roles: [r]`, one key at a time in `meta/main.yml`,
`ansible-playbook -i localhost, play.yml --syntax-check`:

| key in `meta/main.yml`    | result                                                |
| ------------------------- | ----------------------------------------------------- |
| `when: true`              | `'when' is not a valid attribute for a RoleMetadata`  |
| `standalone: true`        | `'standalone' is not a valid attribute for a RoleMetadata` |
| `tags: [foo]`             | `'tags' is not a valid attribute for a RoleMetadata`  |
| `author: someone`         | `'author' is not a valid attribute for a RoleMetadata` |
| `frobnicate: yes`         | `'frobnicate' is not a valid attribute for a RoleMetadata` |
| `become: true`            | clean                                                 |
| `collections: [...]`      | clean                                                 |

One raise site, one message shape, the key name substituted in — so there is no per-key
message to invent. The class name is `self.__class__.__name__` (`base.py:219`), which is why
the string says `RoleMetadata` and why ours must too, broken article included: our existing
arms already emit `for a IncludeRole` rather than `an` (`attributes.rs:350-352`), because the
value of the message is that it is the same string the user gets from ansible.

`tags:` and `author:` are the two most likely real hits — `author` is a `galaxy_info`
sub-key that people hoist to the top level, and `tags` looks legal everywhere else. Both are
fatal at syntax-check, before a task runs.

### Nothing widens the set at runtime

Checked, because a static list is only the answer if nothing appends to it:

- `fattributes` is a read-only `_ClassProperty` computed over the MRO (`base.py:91-105`) and
  is never assigned anywhere in `lib/`.
- `RoleMetadata` overrides neither `preprocess_data` nor `load_data`. This is the escape
  hatch that does exist for `Play`, which renames `user:` to `remote_user` in
  `preprocess_data` *before* validation (`play.py:166-174`) — absent here, so the legal set
  really is the fattributes set.
- `_validate_attributes` (`base.py:211-220`) raises unconditionally. There is no
  `INVALID_TASK_ATTRIBUTE_FAILED`-style downgrade for this class, which is why the rule is
  always ERROR and never a warning.

Oracle re-run for this ticket, agreeing on both cores (27 on 2.21.3, 27 on the
`~/ansible_source` checkout at 2.22.0.dev0):

```
allow_duplicates any_errors_fatal argument_specs become become_exe become_flags
become_method become_user check_mode collections connection debugger dependencies diff
environment galaxy_info ignore_errors ignore_unreachable module_defaults name no_log port
remote_user run_once throttle timeout vars
```

### Why junk keys are in the wild: three readers, one schema

The same file is parsed by three consumers and only one validates:

| reader                          | what it reads              | validates?                      |
| ------------------------------- | -------------------------- | ------------------------------- |
| `role/__init__.py:265`          | the whole mapping          | yes — the 27, fatal             |
| `cli/doc.py:166-208`            | `argument_specs` only      | no                              |
| `galaxy/role.py:125`            | bare `yaml_load`           | **no validation at all**        |

So an extra key survives `ansible-galaxy install` and dies under `ansible-playbook`. That is
the mechanism by which a published role carries a key that breaks on use, and it is the
argument *for* the diagnostic: the editor is the first place the mistake can surface.

### Our side, measured

A probe role with `when:`, `standalone:` and `frobnicate:` in `meta/main.yml` returns **0
diagnostics** today, and `ctx.role_dir` comes back already populated — so the hook the
Approach names exists and is only unrouted.

## Approach

File-kind by path, not content: `FileContext` already knows `role_dir`; a file at
`<role>/meta/main.yml` whose top level is a mapping is the RoleMetadata context. Check its
top-level keys through the existing `legal_key(KeyContext::RoleMetadata, _)` and emit via
`attributes.rs` — always ERROR, there is no `INVALID_TASK_ATTRIBUTE_FAILED` escape for
this class. Values stay out of scope: `dependencies:` entries are RoleInclude-shaped and
`argument_specs:` has its own schema (T-149); this ticket is top-level keys only.

## Done when

- [x] the measured table above is a table-driven test: each of the seven rows asserted by
      key, with the **exact** message string for the five that fire and emptiness for the
      two that do not. The clean rows are the control — a rule that flags everything passes
      the first five on its own — `role_metadata_keys_match_the_live_ansible_verdicts`,
      plus `every_legal_role_metadata_key_stays_silent` over all 27, which is what catches a
      rule wired to the wrong `KeyContext`
- [x] an unknown key gets the ERROR on the **key span**, with the one-edit suggestion —
      `dependencie:` suggests `dependencies`
- [x] every `Base` key (`become:`, `vars:`, …) stays silent there
- [x] a file named `meta/main.yml` outside a role dir, and mapping-shaped files elsewhere,
      are untouched — `only_a_role_s_own_meta_main_is_validated_as_role_metadata`. The row
      that gives that test teeth is `<role>/vars/main.yml`: also a top-level mapping under
      the role, but its keys are *variable names*, so a predicate that checks the filename
      and forgets the directory turns every one into a false ERROR. A `tasks/main.yml` row
      cannot catch it — a task file is a sequence, so the non-mapping guard hides the bug
- [x] `# noqa: invalid-attribute` suppresses it
- [x] a demo role carries BAD rows and a SILENCED row, pinned by
      `the_role_metadata_demo_reports_exactly_its_bad_rows` —
      `demo/roles/metadata-keys/meta/main.yml`, three errors and a `# noqa`'d `standalone:`.
      The eight pre-existing demo `meta/main.yml` files are `dependencies:`-only and stay
      clean, so they remain the free control
- [x] corpus gate, named trees and commits in the test doc-comment like T-184's:
      `role_metadata_corpus`, env-gated on `ANSIBLE_CORPUS`. Measured — kubespray `46dbdd3`,
      debops `65b66ff`, ansible-role-mysql `0a0ea6b`: **0 hits**, against a `demo/` control
      that reports exactly 3. `ansible/ansible-examples` `b505865` has no role `meta/` at
      all and proves nothing. So the rule is silent on well-formed public trees — worth
      recording rather than discovering later, and the reason it is worth having anyway is
      the three-readers split above: the trees that would carry a bad key are the
      hand-written ones, not the published ones

## Note on the corpus counts

`workspace::yaml_files` follows directory symlinks, and both large trees alias their whole
role tree (`extra_playbooks/roles -> ../roles`, `debops/roles -> ansible/roles`), so
kubespray's 31 meta files are walked as 62 and debops' 203 as 812. Correct for resolution —
Ansible loads through those paths too — and harmless for the gate, whose job is to read each
hit rather than to count them. It only affects how a file count reads as coverage, so the
gate's table says "62 (31 distinct)" rather than 62.
