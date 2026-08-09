# T-117 — when: is strict since 2.19 — audit condition.rs

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P1       | S    | T-114 | T-138      |

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

### The gate: ERROR at 2.19+, WARNING below it

Same two cases on **2.18.6** (isolated `uvx --from ansible-core==2.18.6`):

| Written | 2.18.6 | 2.21.2 |
| ------- | ------ | ------ |
| `when: ""` | **runs, no warning** | fatal |
| `when: "'bad'"` | **runs, no warning** | fatal |

Upstream has no warning tier before 2.19 — the task simply runs, since empty is True and a
truthy non-boolean is accepted. `ALLOW_BROKEN_CONDITIONALS` arrived *with* 2.19 as the escape
hatch, and enabling it is what downgrades the error to a deprecation.

We should not mirror that silence. Pre-2.19 the code works *today* and hard-fails the moment
the user upgrades, with nothing between here and there to tell them. A latent break is worth
saying out loud; that is most of why an editor is better placed than the runtime.

So the severity carries the version and the rule does not fork:

| Detected core | Tier | What it says |
| ------------- | ---- | ------------ |
| ≥ 2.19 | **ERROR** | this fails now |
| < 2.19 | **WARNING** | this works now and dies on upgrade |
| undetected | **WARNING** | correct either way; only understates on 2.19+ |

One rule id, one message plus a clause naming the version. The undetected case needs no separate
policy, which is what makes this better than gating the rule's existence on the version.

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

The version decides the **severity**, not whether the rule runs — see the gate table above.
`AnsibleInstall.version` now exists (T-138, via `<package_dir>/release.py`), and an undetected
version falls to WARNING with no special case.

Diagnostics carry one hardcoded severity today (`main.rs:551-558`), so `Problem` needs a
`severity(version: Option<Version>)` beside `rule_id()`/`message()`. `duplicate_key_diagnostics`
(`main.rs:479-484`) is the precedent for a per-diagnostic severity. The three rules that 2.19
did not change keep their current WARNING whatever the version — only the strictness rules read
it.

## Done when

- [x] each existing `when-*` rule is confirmed against 2.19+ or corrected
- [x] a clause that is a string stripping to empty is reported; null and `[]` stay silent
- [x] a fully-wrapped `when: "{{ x }}"` is a HINT that does not claim the deprecation as fact
- [x] a condition that is a bare literal (`"'bad'"`, a number, a list) is reported
- [x] the strictness rules are ERROR at 2.19+, WARNING below it or undetected, and say which
- [x] the corpus re-run still finds zero broken conditions, or explains what changed

## Outcome

Two rules added — `when-empty` and `when-not-boolean` — and one corrected: on 2.19+
`when-jinja-delimiters` drops to a HINT and its message stops claiming the value "evaluates
twice", which stopped being true in 2.19. `Problem` grew `tier(core)` and a version-aware
`message(core)`; `diagnostics_of` takes the version as an argument (`diagnostics_with`) rather
than reading the global, so the tiers are testable without an install.

**The parser needed fixing first.** The rule false-positived on the demo's own GOOD case: a null
`when:` and `when: ""` both reached us as `Scalar { value: "" }`, and Ansible treats them as
absence and a fatal error. libyaml keeps them apart only in the marks — `""` spans its two
quotes, a null spans nothing — so `Node` gained a `Null` variant and a plain empty scalar builds
that. It is deliberately not folded into `Other`, which means "an alias or something we do not
model"; null is a value YAML has, and conflating the two hides the distinction the rule needs.
That variant is the whole reason the empty rule can be a one-line string test.

Deliberately not done:

- **Embedded templates.** `ALLOW_EMBEDDED_TEMPLATES` defaults `true` and 2.21.2 emitted no
  deprecation for the documented shape. A diagnostic would be louder than the runtime.
- **Non-boolean beyond literals.** `n | length` and `s and s` are equally fatal and need filter
  return types (T-115) and a type model (T-116). Recorded in T-114's parser note.
- **The other four keywords.** `changed_when: "'bad'"` in the demo has a rule waiting for it and
  stays silent until T-141 routes it.

Corpus is clean: no false positive on the 416 real expressions. Worth noting *why* that is weak
evidence and still meaningful — the corpus contains no empty or bare-literal condition at all,
which is itself the point: these shapes do not occur in code that ships.
