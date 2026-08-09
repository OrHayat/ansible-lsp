# T-117 — when: is strict since 2.19 — audit condition.rs

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P1       | S    | T-114 | T-138      |

## Problem

`condition.rs` and its four warning rules were written against pre-2.19 `when:` semantics.
2.19 made conditionals strict, so some of what we classify as fine is now fatal, and some of
what we might warn about is now merely deprecated. Shipped rules that describe the wrong
runtime are worse than absent ones.

What changed, all in `_internal/_templating/_engine.py`:

| Case | Now | Cite |
| ---- | --- | ---- |
| a clause that is a string stripping to empty | **fatal** — "Empty conditional expressions are not allowed." | `:490-514` |
| a non-boolean result | **fatal** — "Conditionals must have a boolean result." | `:562-591` |
| `when: "{{ x }}"` fully wrapped | resolved once, then a string result is allowed as indirection and a non-string result is **deprecated** for 2.23 | `:534-546` |
| `when: x == '{{ y }}'` partial embedding | nominally gated on `ALLOW_EMBEDDED_TEMPLATES`, which defaults **true** | `config/base.yml:78-93` |

The non-boolean check is **partly static**. Most results need evaluation, but a condition whose
whole expression is a literal is provable knowing no variables at all. Live-verified on
ansible-core 2.21.2:

```yaml
- command: echo hi
  changed_when: "'bad'"
```

```
[ERROR]: Task failed: Action failed: A 'changed_when' expression failed: Conditional result
         (True) was derived from value of type 'str' at 'cw2.yml:5:21'.
         Conditionals must have a boolean result.
```

The YAML quoting is invisible to Ansible — it strips it and evaluates the remainder as an
expression. So `changed_when: "bad"` is the *variable* `bad` and fails as undefined instead;
only the inner-quoted form is a literal. Both are fatal, by different mechanisms, and only the
literal one belongs to this ticket.

`ALLOW_BROKEN_CONDITIONALS` defaults `false` (`config/base.yml:63-77`) and is itself slated
for removal in 2.23, so the strict behaviour is the only one worth modelling going forward.

This is P1 not because it adds coverage but because it is a correctness audit of rules that
already ship. T-032 is the ticket that built them; this is the version check they never had.

## Audit — measured on ansible-core 2.21.2

Run as `when:` on a real task, reading the outcome rather than the source. **Three of the
claims above were wrong**, and the corrected table is what this ticket implements.

| Written | Outcome | Note |
| ------- | ------- | ---- |
| `when: ""` | **fatal** | "Empty conditional expressions are not allowed." |
| `when: "   "` | **fatal** | stripped before the check, so whitespace counts as empty |
| `when: ["1 == 1", ""]` | **fatal** | one empty clause anywhere poisons the list |
| `when:` (null) | **runs** | ✗ the ticket said fatal |
| `when: []` | **runs** | `when` is a list field defaulting to empty |
| `when: "{{ b }}"`, b a bool | **runs**, deprecation for 2.23 | the wrapped case, as described |
| `when: "{{ e }}"`, e the string `"1 == 1"` | **runs silently** | ✗ resolved to a string, so it is indirection, not deprecation |
| `when: y == '{{ y }}'` | **runs silently** | ✗ no deprecation emitted, despite `base.yml` promising one |
| `changed_when: "'bad'"` | **fatal** | non-boolean literal |
| `changed_when: n \| length` | **fatal** | int result — needs T-115, out of scope here |
| `changed_when: s and s` | **fatal** | `and` returns its operand, not a bool — out of scope |
| `when: demo_mode = "docker"` | **fatal** | "Syntax error in expression: chunk after expression" — `when-assignment` still true |
| `when: not (demo_mode == "native"` | **fatal** | "Syntax error in expression: unexpected end of template" — `when-unbalanced` still true |
| `when: item.rc == 0`, no loop | **fatal** | "'item' is undefined" — `when-item-without-loop` still true |

So three of the four shipped rules are confirmed untouched by 2.19. Only `when-jinja-delimiters`
describes the wrong runtime, and correcting it is the one change this ticket makes to an
existing rule.

### The gate is binary

Same two cases on **2.18.6** (isolated `uvx --from ansible-core==2.18.6`):

| Written | 2.18.6 | 2.21.2 |
| ------- | ------ | ------ |
| `when: ""` | **runs, no warning** | fatal |
| `when: "'bad'"` | **runs, no warning** | fatal |

There is no warning tier before 2.19 — the task simply runs, since empty is True and a truthy
non-boolean is accepted. `ALLOW_BROKEN_CONDITIONALS` arrived *with* 2.19 as the escape hatch,
and enabling it is what downgrades the error to a deprecation. So the gate has two states, not
three: **silent below 2.19, ERROR at 2.19+**. Anything else would false-positive on code that
genuinely works.

This is also the argument for the rule's value: on 2.18 a `when: ""` runs today and hard-fails
on upgrade, with nothing warning in between.

Three consequences:

**Empty is about the string, not the clause count.** `when:` and `when: []` mean *absence* —
`Conditional.when` is `FieldAttribute(isa='list', default=list)`, so a null value is an unset
one. Only a string that strips to empty is refused. This matters for us because `ast::clauses`
collapses null, `[]` and a genuinely absent `when:` into the same empty vec, so the rule must
test clause *strings* and can ignore the empty-vec case entirely — which is simpler than what
the ticket originally asked for.

**Fully-wrapped is not statically decidable.** Whether `when: "{{ x }}"` deprecates depends on
the runtime type of what `x` resolves to: a string is treated as an indirect expression and
allowed silently, anything else is deprecated. A hint may still be worth emitting, but it must
not claim the deprecation as fact — it is one of two outcomes and we cannot know which.

**The embedded case is not worth a rule yet.** `ALLOW_EMBEDDED_TEMPLATES` defaults `true` and
2.21.2 emitted no deprecation for the documented shape. Diagnosing it today means warning about
something that is silent upstream. Worth re-checking when the default flips.

One find for elsewhere: `base.yml:78-93` describes the embedded-template cases as applying to
"conditionals (for example, ``failed_when``, ``until``, ``assert.that``)" — an upstream citation
that those keywords are conditionals in the same sense as `when:`, which is T-141's premise.

## Approach

The audit above is done. What is left is encoding it: two new rules (empty-string clause,
bare-literal condition) and one correction (the fully-wrapped message and tier), with the
embedded case deliberately not implemented.

Gate anything version-sensitive on the detected ansible-core version. `AnsibleInstall.version`
now exists — T-138 added it by reading `<package_dir>/release.py`. Decide what an *undetected*
version means: the recommendation is to assume 2.19+ for the new rules, since silence about a
fatal is the worse failure, but never to escalate an existing WARNING to ERROR without a
detected version.

Diagnostics carry one hardcoded severity today (`main.rs:551-558`), so `Problem` needs a
`severity()` beside `rule_id()`/`message()`. `duplicate_key_diagnostics` (`main.rs:479-484`) is
the precedent for a per-diagnostic severity.

## Done when

- [ ] each existing `when-*` rule is confirmed against 2.19+ or corrected
- [ ] a clause that is a string stripping to empty is an ERROR; null and `[]` stay silent
- [ ] a fully-wrapped `when: "{{ x }}"` is a HINT that does not claim the deprecation as fact
- [ ] a condition that is a bare literal (`"'bad'"`, a number, a list) is an ERROR
- [ ] version-sensitive rules are gated on the detected ansible-core version
- [ ] the corpus re-run still finds zero broken conditions, or explains what changed
