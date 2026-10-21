# T-221 — Resolve a dotted access into a literal mapping variable

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P2       | M    | T-112 | —          |

## Problem

`{{ db.port }}` is understood exactly as far as `db`. The `.port` half is dropped, and the
tool has nothing to say about it — no hover, no jump, no check — even when `db` is a literal
mapping sitting in a `group_vars` file we have already parsed.

```yaml
# group_vars/all.yml
db:
  port: 5432
  host: localhost
```

```jinja
{{ db.port }}    hover says where `db` is defined; `port` is not mentioned
{{ db.prot }}    silent, though `db` is right here and provably has no `prot`
```

**Nothing is currently wrong** — this is a gap, not a defect. `scan_words` already drops a
name preceded by `.` (`condition.rs:780`), so the property is *not* mistaken for a variable and
`var-undefined` does not fire on it. The root-only rule is deliberate and documented:
`lustre_mount_check.stat.exists` yields `lustre_mount_check`. So this ticket adds a capability
on top of a correct base; it does not undo one.

Neither neighbour covers it:

- [[T-189]] type-checks a **registered result's** sub-keys against the module's `RETURN`
  schema — runtime values, typed by upstream documentation.
- [[T-056]] (done) expands **known-literal** variables into templated paths, navigation only.

This is T-056's rule applied to attribute access rather than to path substitution.

## Approach

`VarDef` stores no value, but its `span` points at the value in the defining file, so the
literal is recoverable by parsing that file at that span — which the workspace already does.

The rule, and the whole ticket is this one sentence: **the root's value is a literal mapping we
parsed, or we say nothing.** Concretely, answer only when

- the root resolves to exactly one definition (several means we cannot know which wins without
  running the play — the ordering rules are [[T-090]]'s and are not settled),
- that definition's value is a literal mapping in the YAML, not a template, not a call, and
- the whole access chain is literal — `db.port`, not `db[key]` and not `db.port[i]`.

Then:

- **Hover** on `port` shows the sub-value and where it is written, the same shape [[T-052]]
  gives the root.
- **Go-to-definition** on `port` jumps to the `port:` line rather than to `db:`.
- **A diagnostic** for a key that provably is not there. Certain, not a guess: the mapping is
  literal and complete in one file, unlike [[T-057]]'s registered-key hint, which must stay soft
  because `RETURN` is not an exhaustive list.

### Must stay silent, and this is the risky half

Each of these is a shape where a confident answer would be wrong, so each wants its own
assertion rather than a shared "we handle literals" claim:

| shape | why silent |
| ----- | ---------- |
| `ansible_facts.hostname` | runtime; `is_injected` already drops the root |
| `st.stat.exists` after `register: st` | a runtime result — [[T-189]]'s, via the module schema |
| `db` built with `combine()` / `default()` / any filter | value is computed, not written |
| `db` defined more than once, or under a `when:` | which definition wins is not decided here |
| `db` whose value is itself `{{ … }}` | template, not a literal |
| `db[key]` or `db['port']` | subscript, not attribute — a separate extraction shape |
| the root is undefined | [[T-065]]'s verdict, not this one's; do not double-report |
| `db.items`, `db.keys`, `db.get`, … | Jinja tries the attribute first, so a name the dict has (`dotted::DICT_ATTRIBUTES`, read from `_AnsibleLazyTemplateDict` on 2.21.3) renders the **method**, never the key — measured |

The tempting version of this ticket — "check every dotted access" — is a false-positive machine,
because most dotted access in real playbooks is on facts and registers whose shape we cannot
know. The corpus gate below is what proves we did not build that.

## Progress 2026-10-21: navigation landed, the diagnostic did not

Measured on 2.21.3 with a literal `db` in `group_vars`: `db.port` and `db.nested.deep` render
their values, `db.prot` fails with "object of type 'dict' has no attribute 'prot'", and
`db.items` renders the dict's `items` method although an `items:` key is written. That last row
was missing from the silence table and is added above. `db._priv` reaches a `_priv:` key.

`dotted::chain_at` finds the `.key` chain after a use, and `dotted::walk` follows it through
literal mappings only. `a_dotted_key_on_a_literal_mapping_hovers_and_jumps_and_guesses_nothing`
asserts hover and jump, plus one silent row per shape: method name, absent key, template, two
definitions, `set_fact`, `register`, injected, filter result, `when:`, subscript. Each guard was
seen failing when removed, except one: a `set_fact` or `register` definition's span is its name,
so `walk` never enters one, and that guard was dropped as dead. Demo: `dotted_access.yml`, pinned
by `the_dotted_access_demo_matches_its_annotations`.

## Landed second: `undefined-key`

Measured on 2.21.3: an unguarded `db.prot` fails the task; `| default(…)`, `| d(…)`,
`is defined`, `is undefined`, a ternary on `db.prot is defined`, `db.prot.x | default(1)` and a
task `when: db.prot is defined` all run; `when: db.prot` fails. `dotted::absent_keys` reports only
the failing shapes, reusing `condition::guard_at` and the `when:` test `var-undefined` uses. It is
a WARNING, as `var-undefined` is: an inventory source we did not read could still replace `db`.

Further silent shapes found while building it: a mapping holding a `<<` merge key or written as an
alias (keys we cannot see), and a root defined by a task's or block's `vars:`. The index lets those
reach reads in other tasks, which Ansible does not, and that is **T-249**: here it would have said
"no key" where the name is undefined. Playbooks only, since a role's caller can pass its own root.

Corpus gate, 2026-10-21: **0 hits** over 45 public repos and the private corpus. The same `scan`
reports the demo's BAD row, so the gate can fire.

## Done when

- [x] hover on the property of a single-definition literal mapping shows its sub-value and the
      file it is written in — `Backend::attribute_hover_at`
- [x] go-to-definition on the property lands on the sub-key's line, not on the root's — both
      answer from `Backend::attribute_at`, so they cannot disagree
- [x] a provably-absent key on such a mapping is diagnosed, with `# noqa` honoured and the rule
      id matched exactly — `undefined-key`; the demo's `# noqa: undefined-key` row is silent
- [x] one assertion per row of the silence table above — a test per shape, not one test for
      the rule (rule 3), each seen failing when its guard is removed —
      `a_missing_key_is_reported_only_where_the_read_fails_for_certain` and the hover/jump test;
      the one guard that could not fail was dropped as dead
- [x] a demo fixture carrying both halves, labelled, and pinned the way
      `demo_exercises_every_problem_and_verdict` pins conditions (rule 4) —
      `the_dotted_access_demo_matches_its_annotations`, `every_other_demo_file_is_free_of_undefined_key`
- [x] corpus gate: run over the public trees and `~/app/ansible`, count the diagnostic's hits
      and read **every one** — 0 hits, with the demo as the control that it can fire
- [x] measured, not asserted from the source: the method names, every guard form, `when:`, merge
      keys and the task-`vars:` scope were run on 2.21.3; the remaining silent shapes (two
      definitions, a template, a filter result) make no claim
