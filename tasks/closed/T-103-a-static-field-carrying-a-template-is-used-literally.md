# T-103 — A static field carrying a template is used literally

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P1       | S    | T-099 | —          |

## Problem

```yaml
- command: whoami
  register: "{{ result_var }}"     # registers a fact literally named "{{ result_var }}"
```

Four field attributes are `static=True` and are never templated: `vars`
(`playbook/base.py:696`), `collections` (`collectionsearch.py:34`), `listen`
(`handler.py:27`) and `register` (`task.py:89`). A fifth field is literal through a
different mechanism: `module_defaults` has a no-op `_post_validate_module_defaults`
(`task.py:178`) whose comment says templating is "handled by args post validation" — true
for the values, false for the keys.

Live-verified on core 2.21.2 — the cases split into fatal and silent:

| Field                  | Measured behavior on 2.21.2                                                                     | Our severity |
| ---------------------- | ----------------------------------------------------------------------------------------------- | ------------ |
| `register: "{{ v }}"`  | **Fatal at parse time**: `Invalid variable name '{{ v }}'` — playbook never runs                 | error        |
| templated `vars:` key  | **Fatal at parse time**: same `Invalid variable name` error                                      | error        |
| `collections` entry    | Runs; warning `"collections" is not templatable ... used "as is"`; entry is a dead ref, plugin lookup silently falls through to the rest of the list. Scalar spelling (`collections: "{{ c }}"`) coerces to a one-element list, same dead ref. On a `roles:` entry: no warning at all | warning |
| `listen: "{{ v }}"`    | **No warning at all**; braces stay literal, so the matching `notify:` fails at runtime with `handler 'x' was not found` — the error names the topic, not the templated `listen` that broke it | warning |
| templated `module_defaults` key | **Fatal before any task runs**: `Could not resolve action ansible.legacy.{{ m }} in module_defaults` — group keys (`"group/{{ g }}"`) equally fatal (`could not resolve the module_defaults group`) | error |

Older cores emitted the `post_validate_attribute` (`base.py:550-557`) "not templatable"
warning for `register` too and shipped the braces into the fact name; 2.21 promoted
`register` and `vars` keys to hard errors via variable-name validation. `keyword_desc.yml:35`
documents only the `listen` case, so three of the four are folklore.

### Measured non-cases — keys that look static but template fine (do NOT flag)

Every row below was live-run on 2.21.2; a template in these keys resolves, so flagging any
of them would be a false diagnostic:

| Key                                  | Mechanism                                                     |
| ------------------------------------ | ------------------------------------------------------------- |
| `loop` / `with_<lookup>`             | deferred — `_post_validate_loop` (`task.py:388`) returns the raw value; TaskExecutor templates it |
| `changed_when` / `failed_when` / `until` | deferred — evaluated after execution                       |
| `delegate_to`                        | deferred — `TaskExecutor._calculate_delegate_to`              |
| `environment`                        | templated per-entry at post-validate                          |
| `include_tasks` / `include_role: name` | dynamic — templated at runtime, `set_fact` vars work        |
| `notify`                             | templated; a templated value matched a literal handler name   |
| handler `name`                       | templated; `notify` on the resolved value matched             |
| `loop_control.loop_var`              | templated (item landed in the resolved var name)              |
| `vars_files` entry                   | templated per host — even a host var works, though play-level processing first emits a spurious "skipping vars_files item due to an undefined variable" warning |
| `module_defaults` **value**          | templated when merged into args at args post-validate (only the keys are literal) |
| `set_fact` **key** (quoted)          | templated — module args post-validate through the templar, keys included; the fact lands under the resolved name |
| `vars_prompt` entry `name`           | templated — the prompted value lands under the resolved name  |
| `tags`                               | templated — a templated tag matched a literal `--tags` filter |
| `strategy` / `serial` / `order`      | templated — play runs under the resolved values               |

### Adjacent, but a different rule (not T-103)

- **`import_tasks` / `import_role: name` / `roles:` entry / `import_playbook` /
  `hosts`** — templated at parse time, so they work only if the var comes from play
  `vars`/`vars_files`/extra-vars. A `set_fact` var, a fact, or an inventory host/group var
  is a **fatal** `'x' is undefined` at parse; core's own error says "Static imports cannot
  use variables from facts or inventory sources like group or host vars."
  `import_playbook` is narrower still: a play var from a preceding play in the same file
  does not reach it — only extra-vars did. This is source-provenance analysis, which is
  T-136's territory, not a literal-use lint.
- **`when:` (and friends) wrapped in `{{ }}`** — still evaluates on 2.21.2 but emits
  a deprecation warning: removal scheduled for core 2.23. Belongs with the conditional
  rules (T-032/T-122), not here.

## Approach

Flag `{{` in the value of those keys. The `static=True` set is closed and comes from the
same `FieldAttribute` tables as T-107, so that part should read the tables rather than
hardcode four names — a fifth static attribute added upstream should light up for free.
`module_defaults` is not in that table (its literalness lives in a no-op post-validator),
so it is the one name the rule carries explicitly — with a comment saying why.

`vars` and `module_defaults` are special the same way: it is the *keys* that are not
templated while values are, so the check belongs on key names there.

Severity follows the matrix: error where Ansible hard-fails before running (`register`,
`vars:` keys, `module_defaults` keys), warning where it runs and silently misbehaves
(`collections`, `listen`). Messages differ the same way: the error cases say the playbook
will not run; the warning cases say the braces end up in the literal value and what that
costs (`listen`: the notify can never match; `collections`: the entry is a dead ref).

The enumeration behind "this is all of them": every path through `post_validate_attribute`
(`base.py:544`) was walked — the `static` flag (the four), all thirteen
`_post_validate_*` overrides (each templates now, defers to execution-time templating, or
is `module_defaults`), and the skip paths (import_role's static skip is T-136 territory).
Play-level corners that bypass post-validation were probed live and template fine:
`vars_prompt` names, `tags`, `strategy`, `serial`, `order` (rows in the non-case table).

Duplicate keys follow keep-last: a discarded first `register: "{{ v }}"` (or first
`vars:` mapping with a templated key) loads clean with only the `Using last defined
value only` warning, while the same template written last is fatal — so the rule judges
only the last occurrence of each static key, the same read `Node::get` takes.

Post-close review corrections, all measured on 2.21.2:

- `import_playbook` entries are PlaybookInclude — no CollectionSearch, so `collections`
  there is a fatal invalid attribute and gets no templating claim; `vars` and
  `module_defaults` keys keep the usual fatals.
- Dynamic includes keep only `VALID_INCLUDE_KEYWORDS`: `module_defaults` on one is
  T-107's invalid-attribute and the resolution failure never happens, so the rule uses
  the DynamicInclude contexts there.
- `module_defaults` also takes a **list** of mappings; a templated key in list form is
  the same fatal. List-form `vars` is fatal for its shape alone (`Vars in a Task must be
  specified as a dictionary`, template or none), so its keys are not flagged.
- `register:` with a **list** value is fatal with or without a template (`Invalid
  variable name of type 'list'`), so the braces are not the fault there — unflagged,
  the shape story is T-108's.

## Done when

- [x] a template in `register`, in a `vars:` mapping **key**, or in a `module_defaults`
      **key** is an error; templated `vars:` / `module_defaults` values are not flagged
- [x] a template in `listen` or a `collections` entry is a warning
- [x] the `static=True` set is read from the keyword tables, not hardcoded;
      `module_defaults` is the one explicit addition
- [x] error messages say the playbook fails at parse time; warning messages say the braces
      end up in the literal value and why that bites
