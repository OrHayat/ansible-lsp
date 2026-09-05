# T-157 — A block or import_tasks in handlers: makes the handler's name unnotifiable

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P2       | S    | T-099 | —          |

## Problem

In every ordinary handler, the list entry's `name:` is what `notify:` looks up. Put a `block:`
or an `import_tasks:` on that entry and it silently stops being that — the name stays on the
page and stops being a handler name.

Measured on ansible-core **2.21.2**, all three `--syntax-check` clean (exit 0):

| Handler entry                | `notify:` the entry's name | What is notifiable instead          |
| ---------------------------- | -------------------------- | ------------------------------------ |
| ordinary task                | works                      | —                                    |
| `include_tasks:`             | **works**                  | the entry, and the included names too |
| `block:`                     | **fails**                  | the names of the tasks inside it      |
| `import_tasks:`              | **fails**                  | the names inside the imported file    |

```yaml
handlers:
  - name: h            # inert — a label on the Block, and nothing looks it up
    block:
      - name: inner    # THIS is the handler
        debug: {msg: x}
```

`notify: h` → `The requested handler 'h' was not found in either the main handlers list nor in
the listening handlers list`. `notify: inner` runs it.

A Block is not a Handler: it flattens into its child tasks, and those are what land in the
handler list, each under its own name. `import_tasks:` is expanded at parse time and behaves
the same way. `include_tasks:` is the one that keeps working, because the include itself stays
in the handler list as a notifiable entry.

Two things make this worse than a naming quirk:

- **The inner tasks usually have no `name:`.** People write `- name: h` on the entry and leave
  the body unnamed, since that is how handlers normally work. Then *nothing* is notifiable and
  the handler is dead code.
- **It only ever complains at run time, and only sometimes.** `ERROR_ON_MISSING_HANDLER` fires
  from the notifying task, so it needs that task to actually report `changed`. An unchanged
  task hides the whole thing — the same gap `upstream/ansible-missing-handler.md` documents.
  A playbook can carry a dead handler for years and fail the first time the notifier changes.

Surfaced twice while working T-110 (`9ccc85c` scoped it, row 1 met it again) and both times it
was set aside as not-a-placement-rule, because ansible-core raises nothing at load. It is not a
T-110 row for exactly that reason: there is no upstream error to replicate.

## Approach

A shape test on a play's `handlers:` entries — no resolution, no index. For each entry, decide
whether the entry's own `name:` survives as a handler name:

- entry has `block:`/`rescue:`/`always:` → it does not
- entry's action is `import_tasks` (any spelling) → it does not
- otherwise → it does

WARNING, not an error: the playbook loads and runs, and if nobody notifies the lost name
nothing goes wrong. Its own rule id, since ansible-core is silent here — the same footing as
`shadowed-loop` and `dead-loop-control` in `placement.rs`.

The message has to carry the mechanism or it reads as a style nit. It should name what is
notifiable *instead*: the inner task names when they exist, and when they do not, say that
nothing is.

Sharper still, and cheap once T-028 lands: only warn when some `notify:` in the play actually
names the lost handler. That turns a shape warning into a provable fault, and drops it to
nothing on playbooks that never notify it. Worth doing as a follow-up rather than a blocker —
the standalone warning is useful now and T-028 is `M`.

Where it lives is an open question. It is not placement (nothing is misplaced) and not
attributes (every key is legal). Either a new module or a `handlers.rs` that batch 3's
remaining rows could share.

## Done when

- [x] a `block:` handler entry warns that its `name:` is not notifiable, naming the inner
      task names that are
- [x] the same for an `import_tasks:` handler entry, FQCN spellings included
- [x] an entry whose body has no names at all says so — nothing in it can be notified
- [x] `include_tasks:` entries and ordinary handlers stay silent, asserted both ways
- [x] its own rule id, `# noqa`-suppressible per T-010, WARNING severity
- [x] a demo fixture carries all four rows of the table above, good and bad

## Landed

`dead-handler-name`, in `placement.rs` beside the other rules that are ours rather than
ansible-core's, anchored on the entry's `name:` value. A block entry names the inner task
names that *are* notifiable, or says nothing inside is named; an `import_tasks` entry (any
core spelling) points at `include_tasks`. Ordinary handlers and `include_tasks` entries are
silent, and so is a named block in `tasks:` or in a standalone file, where it is ordinary
Ansible. `# noqa: dead-handler-name` on the name line suppresses it; `invalid-placement` does
not.

The measurement was re-run rather than trusted (`notify: h` against each entry shape on
2.21.2): ordinary and `include_tasks` run `h`; `block`, `import_tasks` and
`ansible.builtin.import_tasks` fail with `handler 'h' was not found`; the names inside run.

### The tier is ansible's, not a fixed WARNING

The ticket said WARNING because "the playbook loads and runs, and if nobody notifies the lost
name nothing goes wrong". Measured against the demo file, that undersells the notified case:

| `ERROR_ON_MISSING_HANDLER` | a notified dead name at run time | exit |
| --- | --- | --- |
| on (default) | `[ERROR] The requested handler 'reload proxy' was not found` — the run dies | 1 |
| off | one `[WARNING]` per missing name, the live handlers still run | 0 |
| either | `--syntax-check` clean; an unchanged notifier reports nothing | 0 |

So the setting is read the way ansible reads it — `AnsibleConfig::error_on_missing_handler`,
ini `[defaults] error_on_missing_handler`, env `ANSIBLE_ERROR_ON_MISSING_HANDLER`, env over
ini, default true, all confirmed with `ansible-config list` and a run with the two set against
each other — and the tier follows it:

- the play notifies the dead name by a literal `notify:` (a list item, inside a block, on a
  block, or from another handler) and no live handler name or `listen:` topic answers it →
  proven: **ERROR** with the setting on, **WARNING** with it off, and the message says which;
- nothing in the play notifies it, or only a templated `notify:` does → dead code, **WARNING**
  either way.

Pinned in `a_handler_name_on_a_block_or_an_import_is_reported_as_dead` (every shape, both
settings, the twin and `listen:` controls), `error_on_missing_handler_reads_ini_and_env`, and
in `main.rs` by the demo pin (three ERRORs and one WARNING on `demo/dead_handler_names.yml`,
every other demo file counted — `placement.yml` carries seven by construction) plus a fixture
whose `ansible.cfg` flips the tier. Each was broken and seen red: setting ignored, live names
ignored, config not threaded.

The "only warn when some `notify:` names it" follow-up the Approach deferred to T-028 turned out
not to need it: the play's own `notify:` values are in the parse tree, and that is exactly the
evidence the tier rests on. What stays unknowable — a `notify:` inside an included file — only
ever withholds the ERROR, never invents one.
