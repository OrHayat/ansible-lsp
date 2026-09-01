# T-221 — Resolve a dotted access into a literal mapping variable

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-112 | —          |

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

The tempting version of this ticket — "check every dotted access" — is a false-positive machine,
because most dotted access in real playbooks is on facts and registers whose shape we cannot
know. The corpus gate below is what proves we did not build that.

## Done when

- [ ] hover on the property of a single-definition literal mapping shows its sub-value and the
      file it is written in
- [ ] go-to-definition on the property lands on the sub-key's line, not on the root's
- [ ] a provably-absent key on such a mapping is diagnosed, with `# noqa` honoured and the rule
      id matched exactly
- [ ] one assertion per row of the silence table above — a test per shape, not one test for
      the rule (rule 3), each seen failing when its guard is removed
- [ ] a demo fixture carrying both halves, labelled, and pinned the way
      `demo_exercises_every_problem_and_verdict` pins conditions (rule 4)
- [ ] corpus gate: run over the public trees and `~/app/ansible`, count the diagnostic's hits
      and read **every one**. A provable key error should be rare; a hit that is not one is a
      false positive and blocks the ticket
- [ ] measured, not asserted from the source: build a mapping under each silent shape above and
      confirm against the installed core what the value actually is before claiming we cannot
      know it (rule 1)
