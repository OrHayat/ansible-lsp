# T-107 — Per-class keyword sets from FieldAttribute

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P1       | M    | T-106 | —          |

## Problem

The legal keyword set is different for a play, a block, a task, a handler, a `roles:` entry
and `meta/main.yml`, and an unknown key is `AnsibleParserError` — `'%s' is not a valid
attribute for a %s` (`playbook/base.py:211-220`). `keywords.rs` has one flat list, so we
cannot say a keyword is legal *here*.

**Do not build this from `keyword_desc.yml`.** It is a docs artefact: it omits `listen`,
`validate_argspec` and `local_action`, and it documents `handlers:`/`collections:` prose that
has no corresponding key. The schema is the `FieldAttribute` declarations, gathered over the
MRO (`base.py:93-105`).

Class composition is where the errors live:

| Class          | Mixes in                                                        | Cite |
| -------------- | --------------------------------------------------------------- | ---- |
| `Play`         | `Base`, `Taggable`, `CollectionSearch` **only**                  | `play.py:48,61-89` |
| `Block`        | + `Conditional`, `Taggable`, `CollectionSearch`, `Delegatable`, `Notifiable` | `block.py:35-37` |
| `Task`         | + `args action async changed_when delay failed_when loop loop_control poll register retries until` | `task.py:79-94` |
| `Handler`      | Task + `listen`                                                  | `handler.py:27` |
| `RoleInclude`  | `Base`+`Conditional`+`Taggable`+`CollectionSearch`+`Delegatable`+`role` | `definition.py:41-43` |
| `RoleMetadata` | `Base`+`CollectionSearch` + `allow_duplicates dependencies galaxy_info argument_specs` | `metadata.py:32-41` |

So `when:`, `notify:`, `delegate_to:`, `loop:`, `register:` and `block:` are each **fatal at
play level**, and `loop:` on a `block:` is fatal — a common mistake, since blocks look like
tasks. Conversely `become: true` in `meta/main.yml` parses fine and does nothing, while the
ansible-lint idiom `standalone:` there is a hard error.

For tasks specifically the severity is config-driven: `INVALID_TASK_ATTRIBUTE_FAILED`
defaults True, else the key is downgraded to `Ignoring invalid attribute` — `task.py:332-342`,
`config/base.yml:1703-1713`. A key is only "unknown" after `with_<lookup>` and module names
are excluded (`mod_args.py:128-132`).

T-088 is the play half of this, filed before the rest was understood; it closes when this
does.

## Approach

Transcribe the six tables once, with the mixin composition explicit rather than flattened, so
the next ansible-core version can be diffed against it. Extend `keywords.rs` from a list to a
per-class set.

## Done when

- [ ] each of the six contexts has its own legal set, composed from named mixins
- [ ] `when:` on a play and `loop:` on a block are ERRORs
- [ ] task-level severity follows `INVALID_TASK_ATTRIBUTE_FAILED`
- [ ] module names and `with_*` are excluded before a key is called unknown
- [ ] T-088 closes as part of this
