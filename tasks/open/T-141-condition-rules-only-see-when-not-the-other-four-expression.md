# T-141 — Condition rules only see `when:`, not the other four expression keywords

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-114 | T-140      |

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

- [ ] `failed_when`, `changed_when`, `until` and `assert: that:` all reach `condition::problems`
- [ ] a diagnostic names the keyword it fired on, not `when:` unconditionally
- [ ] rule ids stay stable for `when:` so existing `# noqa:` comments keep working
- [ ] the demo carries a BAD case for at least one non-`when` keyword
- [ ] the corpus test shows no new false positives on the collections and kubespray
