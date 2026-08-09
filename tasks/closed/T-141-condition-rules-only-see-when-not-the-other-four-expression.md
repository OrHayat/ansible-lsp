# T-141 — Condition rules only see `when:`, not the other four expression keywords

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | M    | T-114 | T-140      |

## Symptom

Every condition rule we have — `when-assignment`, `when-unbalanced`, `when-jinja-delimiters`,
`when-item-without-loop` — fires on `when:` and on nothing else. Ansible has five bare-expression
keywords, and the other four are unlinted:

| Keyword | Linted today |
| --- | --- |
| `when:` | yes |
| `failed_when:` | **no** |
| `changed_when:` | **no** |
| `until:` | **no** |
| `assert:` → `that:` | **no** |

They are not lesser cases. Live-verified on ansible-core 2.21.2, the same broken expression is
equally fatal in either place:

```yaml
- command: echo hi
  failed_when: demo_mode = 'docker'
```

```
[ERROR]: Task failed: Action failed: A 'failed_when' expression failed:
         Syntax error in expression: chunk after expression
fatal: [localhost]: FAILED!
```

Identical error to the `when:` form, identical outcome — the task dies. We warn on one and
stay silent on the other four, which is worse than warning on none: it teaches that silence
means "checked and fine".

## Cause

Nothing types a directive's value. `ast::Directive` is

```rust
pub struct Directive { pub key: String, pub key_span: Span, pub value: Span }
```

— a name and two spans, with no notion that some values are Jinja expressions and others are
literals. `references.rs` then hardcodes the one key it cares about:

```rust
r.conditions = t.when.clone();          // references.rs:200, 230
directives.iter().find(|d| d.key == "when")  // references.rs:192, for condition_span
```

`keywords.rs` already lists `changed_when`, `failed_when` and `until` as valid keyword names
(lines 52-63), so they parse and are accepted — they are simply never read as expressions.

## Fix

Mark which keys are bare-expression contexts and route all of them into the existing rules.
No Jinja parser needed; this is about *which* strings reach `condition::problems`, not how
they are analysed.

- A `BARE_EXPRESSION` set beside the keyword tables: `when`, `failed_when`, `changed_when`,
  `until`, plus `that` when it sits under `assert:`.
- `Reference` needs to carry which keyword a condition came from, so a diagnostic can say
  `failed_when` rather than always `when`. The rule ids and messages are `when-`prefixed and
  hardcode "when:" in their text (`condition.rs:253-272`) — both need the keyword threaded
  through, or the message will name the wrong key.
- `assert: that:` is a *list* of expressions and can also be a lone string; both forms feed
  the same rules, like `when:` already does.
- `until:` implies `retries`/`delay` but that is a separate check — not this ticket.

**Do T-140 first.** It is the reason for the `Depends on`: the Jinja-keyword-argument false
positive currently fires on `when:` alone, and widening the input set before fixing it
multiplies that bug across four more keywords.

## Done when

- [x] `failed_when`, `changed_when`, `until` and `assert: that:` all reach `condition::problems`
- [x] a diagnostic names the keyword it fired on, not `when:` unconditionally
- [x] rule ids stay stable for `when:` so existing `# noqa:` comments keep working
- [x] the demo carries a BAD case for at least one non-`when` keyword
- [x] the corpus test shows no new false positives on the collections and kubespray

## Outcome

A new `expressions` module owns *which* strings are expressions: `BARE_EXPRESSION` plus
`sites(&[Node]) -> Vec<Site>`, each site carrying the keyword, both spans, the clauses and the
owning task's `has_loop`. `condition::problems` is unchanged in what it analyses.

**The carrier changed, which was the real work.** The rules used to read `Reference.conditions`,
so a condition was only diagnosed if its task produced a *file reference*. The plan's guess —
that `Reference` should carry the keyword — was wrong: `conditions` means "gates this
reference", and `failed_when` is not a gate. Reading the tree instead fixed three things at
once: all five keywords, tasks that reference nothing, and **block-level `when:`**, which
`references.rs:213` had never diagnosed at all. `Reference.conditions` stays for the hover,
inlay hints and the import-mutation check, which are about gating.

Sites come from the raw tree rather than `ast`, because `ast::Directive` keeps only spans and
the rules need clause text. A file that is neither playbook nor tasks yields nothing, and
`vars:`/`set_fact:` subtrees are skipped, so a vars file with a key called `when` stays data.

### The corpus run found three rule bugs, not one

Measured end to end — our parser, our site walk, real `has_loop` — over the four pinned
collections plus kubespray: **1195 files, 3027 sites, 8097 clauses**. Thirteen hits, of which
eleven were false positives that `when:` alone had never reached:

| Shape | Was reported | Why it was wrong |
| ----- | ------------ | ---------------- |
| `x == "the label's value"` | `when-unbalanced` ×3 | quote *parity* counts an apostrophe inside a double-quoted string; now tracks which quote is open |
| `map(attribute = "x")` | `when-assignment` ×1 | T-140 required the name to touch the `=`; kubespray spaces it |
| `'{{ host }}' == x`, `{{ a }}:{{ b }} in xs` | `when-jinja-delimiters` ×7 | embedded templating, which `ALLOW_EMBEDDED_TEMPLATES` allows by default with no deprecation (T-117's audit); only the *fully wrapped* form is the real case |

After the fixes: **2 hits, both correct** — genuinely wrapped `assert: that:` expressions in
kubespray, which is the deprecated-for-2.23 shape and reports as a HINT. All five shapes are
pinned verbatim in `shapes_the_corpus_proved_are_not_faults`.

Worth naming: all three bugs predate this ticket and would have fired on `when:` too. Widening
the input set is what surfaced them, which is the argument for the gate being end-to-end rather
than a list of strings — the first harvest mangled multi-line scalars and invented nine
failures that did not exist.

`until:` implying `retries`/`delay` is still not checked — out of scope, as written above.
