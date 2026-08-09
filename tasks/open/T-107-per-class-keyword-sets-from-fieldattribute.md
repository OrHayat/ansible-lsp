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
| `RoleInclude`  | `Base`+`Conditional`+`Taggable`+`CollectionSearch`+`Delegatable`+`role` | `definition.py:41-43`, `include.py:28` |
| `RoleMetadata` | `Base`+`CollectionSearch` + `allow_duplicates dependencies galaxy_info argument_specs` | `metadata.py:32-41` |

## Node types and their legal sets

The mixins, with their exact keys (verified against `lib/ansible/playbook/*` in the source
checkout; each list is the `FieldAttribute`/`NonInheritableFieldAttribute` declarations, and
the YAML key is the attribute name unless an `alias=` says otherwise):

| Mixin              | Keys                                                                     | Cite |
| ------------------ | ------------------------------------------------------------------------ | ---- |
| `Base`             | `name connection port remote_user vars module_defaults environment no_log run_once ignore_errors ignore_unreachable check_mode diff any_errors_fatal throttle timeout debugger become become_method become_user become_flags become_exe` (22) | `base.py:688-721` |
| `Conditional`      | `when`                                                                   | `conditional.py:32` |
| `Taggable`         | `tags`                                                                   | `taggable.py:46` |
| `CollectionSearch` | `collections`                                                            | `collectionsearch.py:34` |
| `Delegatable`      | `delegate_to delegate_facts`                                             | `delegatable.py:10-11` |
| `Notifiable`       | `notify`                                                                 | `notifiable.py:10` |

The six node types, as mixin sums plus their own keys:

| Node           | = mixins                                                              | + own keys |
| -------------- | --------------------------------------------------------------------- | ---------- |
| `Play`         | `Base + Taggable + CollectionSearch`                                  | `hosts gather_facts gather_subset gather_timeout fact_path vars_files vars_prompt validate_argspec roles handlers pre_tasks post_tasks tasks force_handlers max_fail_percentage serial strategy order` (18, `play.py:61-89`) |
| `Block`        | `Base + Conditional + Taggable + CollectionSearch + Delegatable + Notifiable` | `block rescue always` (`block.py:35-37`) |
| `Task`         | same six mixins as `Block`                                            | `args action async changed_when delay failed_when loop loop_control poll register retries until` (12, `task.py:79-94`; `async` is the YAML alias of `async_val`) |
| `Handler`      | `Task`                                                                | `listen` (`handler.py:27`) |
| `RoleInclude`  | `Base + Conditional + Taggable + CollectionSearch + Delegatable`      | `role` (`definition.py:43`) |
| `RoleMetadata` | `Base + CollectionSearch`                                             | `allow_duplicates dependencies galaxy_info argument_specs` (`metadata.py:38-41`) |

The transcription above is **verified against Ansible's own machinery, not by grep**: the
unknown-key error has exactly one raise site, `_validate_attributes` (`base.py:211-220`),
which checks membership in `frozenset(self.fattributes)` — and `fattributes` is built
mechanically over the MRO. So the ground truth is executable:

```
python3 -c "import sys; sys.path.insert(0,'lib'); \
  from ansible.playbook.play import Play; print(sorted(Play.fattributes))"
```

run from the source checkout, for each of the six classes. A mechanical diff of that output
against the tables above matches exactly. Re-run it when bumping the snapshot to a newer
ansible-core — do not re-grep.

One subtlety the oracle exposed: `fattributes` keys are attribute **names and aliases
both** — `Task.fattributes` contains `async` *and* `async_val` *and* `loop_with`, and
`_validate_attributes` accepts any raw key. Verified empirically:
`Task.load({'debug': None, 'async_val': 5, 'loop_with': 'items'})` loads clean, while
`frobnicate` raises `'frobnicate' is not a valid attribute for a Task`. So the legal set
for Task/Handler must include `async_val` and `loop_with`, or we false-positive on YAML
that Ansible accepts.

Not `FieldAttribute`s but still legal, handled before the unknown-key check:

- the **module key** itself on a task (any non-directive key; FQCN always) — `mod_args.py:330`
  excludes `_task_attrs` and `with_*` before looking for the module
- `local_action` — added to `_task_attrs` at `mod_args.py:131`; implies `delegate_to`, and
  `action` + `local_action` together is fatal (`mod_args.py:318-322`)
- `with_<lookup>` — prefix match **plus** `removeprefix("with_") in lookup_loader`
  (`task.py:336`): `with_items` becomes `loop`/`loop_with`, but `with_nosuchlookup` falls
  through to the same `INVALID_TASK_ATTRIBUTE_FAILED` branch as any unknown key
  (`task.py:339-342`). `loop_with` itself is private, never a YAML key (`task.py:94`)

**Legacy escape hatch on plays** (found by reading `play.py` whole, invisible to both grep
and the fattributes oracle): `Play.preprocess_data` renames `user:` to `remote_user` *before*
`_validate_attributes` runs (`play.py:166-174`), so `user:` is legal on a play — fatal only
when both `user` and `remote_user` are written.

**Include tasks are a seventh context with a *smaller* set.** `TaskInclude.preprocess_data`
(`task_include.py:87-99`) restricts **dynamic** includes — tasks whose action is
`include_tasks` or `include_role` (`constants.py:45`) — to `VALID_INCLUDE_KEYWORDS`
(`task_include.py:42-44`), just 16 keys:
`action args collections debugger ignore_errors loop loop_control loop_with name no_log
register run_once tags timeout vars when` — plus `listen` in handler context
(`handler_task_include.py:27`). So `become:`, `delegate_to:`, `changed_when:`, `until:`,
`retries:` on an `include_tasks`/`include_role` task are each **invalid**, with the same
`INVALID_TASK_ATTRIBUTE_FAILED` severity split. `import_tasks`/`import_role` skip this
check and keep the full Task set. (Unwritten keys can't trip it: the parser leaves e.g.
`delegate_to` as `Sentinel` when absent, `mod_args.py:303`, and the check skips Sentinel.)

**`IncludeRole` declares four task-level attributes of its own** — `public`,
`allow_duplicates`, `rolespec_validate`, `rescuable` (`role_include.py:47-52`). Because of
the dynamic-include restriction above they are only *reachable* as task-level keys on
`import_role`; on `include_role` they must be args. They are also args in
`OTHER_ARGS` either way.

**`loop_control:` is a nested context of its own.** Its value loads as `LoopControl`, which
extends `FieldAttributeBase` directly — *not* `Base` — so its legal set is exactly
`loop_var index_var label pause extended extended_allitems break_when`
(`loop_control.py:27-35`), no common keywords, and an unknown key there raises
`'%s' is not a valid attribute for a LoopControl` through the same `_validate_attributes`.

**Include args are closed sets of their own** (validated after load, hard errors):
`include_tasks`/`import_tasks` args are `file` + free-form, plus `apply` on `include_tasks`
only (`task_include.py:39-44,70-83`). `include_role`/`import_role` args are the 11-key
`VALID_ARGS` (below).

The two role contexts have **different unknown-key semantics** — neither is the plain
`AnsibleParserError` path:

- `include_role`/`import_role` **args** are closed: `VALID_ARGS = name role tasks_from
  vars_from defaults_from handlers_from apply public allow_duplicates rolespec_validate
  rescuable` (`role_include.py:40-43`); anything else is a hard `Invalid options` error
  (`role_include.py:137-139`), and `apply`/`rescuable` are additionally fatal on
  `import_role` (`role_include.py:150-159`). Today's `ROLE_INCLUDE_KEYS` in `keywords.rs`
  is missing `apply` and `rescuable`.
- a **`roles:` play entry is open**: any key not in the `RoleInclude` fattributes is split
  off as a *role param* — an inline variable passed to the role, fully legal
  (`definition.py:200-224`, `- {role: x, myvar: 1}`). Unknown keys there must never be
  flagged.

Note what the sums *exclude* — that is where the diagnostics come from: `Play` has no
`Conditional`/`Delegatable`/`Notifiable`, so `when:`/`delegate_to:`/`notify:` on a play are
fatal; no node except `Block` has `block:`, so `loop:` on a block and `block:` in a play's
task position are each fatal; `RoleMetadata` has no `Conditional`, so `when:` in
`meta/main.yml` is fatal — while all 22 `Base` keys (`become:` included) parse there fine.

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
