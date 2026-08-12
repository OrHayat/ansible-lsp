# T-169 — A template inside a mapping key is invisible to the var walk

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | task | P2       | S    | —          |

## Problem

```yaml
vars:
  result_name: my_result
tasks:
  - ansible.builtin.set_fact:
      "{{ result_name }}": true
```

This creates a fact named `my_result` — the supported spelling for dynamic fact names
(unlike the `vars:` keyword, whose keys are static and fatal — T-103).

Our var walk sees none of it. Three faults, one root:

1. `walk_uses` (`vars.rs:300-311`) scans mapping **values** only; `k` is consulted just to
   detect `when:`. The `result_name` use inside the key gets no hover, no
   go-to-definition, and no undefined check.
2. The `set_fact` indexer (`vars.rs:394-403`) records each key **literally**, so the index
   gains a definition named `{{ result_name }}` — a name no expression can ever
   reference (braces are not valid variable-name characters).
3. The fact actually created (`my_result`) is not indexed under any name.

Today only fault 1 is user-visible (nothing can reference a brace-name, so the bogus
definition stays latent — which is why this is a task, not a bug). Fault 2 becomes
user-visible the moment anything enumerates definitions: completion (T-127), the reverse
index (T-020/T-113).

### Correction: this is two sites, not "module args"

Filed as "module args template their keys too". Measured on 2.21.2, that is **false**, and
implementing it would have been a new class of wrong answer:

| spelling                                            | result                                              |
| --------------------------------------------------- | --------------------------------------------------- |
| `set_fact: {"{{ result_name }}": true}`             | fact `my_result` — rendered                          |
| `set_stats: {data: {"{{ k }}_stat": 1}}`            | stat `dynamic_stat` — rendered                       |
| `debug: {"{{ argname }}": x}`                       | **fatal** `Unsupported parameters … {{ argname }}`   |
| `set_fact: {outer: {"{{ k }}": 1}}`                 | key stays `{{ k }}` — never rendered                 |

Exactly two action plugins render a key, and upstream marks both:
`k = self._templar.template(k)  # a rare case where key templating is allowed`
(`plugins/action/set_fact.py:44`, and `set_stats.py:62` for the `data:` half). Everywhere
else the braces are literal text, so a use scanned there would hover and Cmd+click a name
the run never resolves — on a line Ansible refuses outright, in the arg-key case.

The generic reading also mattered for fault 2's fix: keys are skipped in the indexer
because they are *templated*, which is only true at those two sites; a literal key with
braces elsewhere is a real (if unusable) name and not this rule's business.

## Approach

- `walk_uses`: carry a three-state `Keys` marker (`Literal` / `Templated` / `UnderData`)
  and scan scalar keys only where Ansible renders them, with the same guard accumulation
  as values. The openers fire only from `Literal`, so a fact *named* `set_fact` cannot
  open a second templated level.
- `set_fact` indexer: skip keys containing `{{` rather than recording the literal text.
  Fault 3 is knowingly left as a miss for now — resolving the defined name requires
  evaluating the template, which is T-034's statically-knowable-templating machinery;
  note it there rather than half-doing it here.
- Rule 3 applies: uses feed hover, undefined_uses and the future reverse index — a test
  per consumer, not one for the walk.

## Done when

- [x] hover and go-to-definition work on `result_name` inside a `set_fact` key, and inside
      `set_stats`' `data:` — the two sites that replace "an arbitrary module-arg key",
      which is measured fatal rather than rendered
- [x] a name in a key Ansible leaves literal gets neither view — asserted on the same demo
      file, so the two halves cannot drift apart
- [x] `undefined_uses` sees a use inside a mapping key (test with an undefined name in a
      key, and a defined one as the control)
- [x] a templated `set_fact` key no longer produces a literal `{{ ... }}` definition in
      the index
- [x] a literal `set_fact` key still indexes as a definition (the existing behaviour, as
      the control)
- [x] the dynamic-name miss (fault 3) is recorded in T-034
- [x] the corpus's own two spellings (`{{ item.key }}` under a `loop:`, and a
      `default(...)` fallback) are pinned silent, with the bare name as the control

## Landed

`vars.rs` (the `Keys` marker + the indexer skip), five core tests, one LSP test covering
hover and go-to-definition together, and three demo rows in `demo/tasks/variables.yml`
(GOOD `set_fact`, GOOD `set_stats`, NO HINT nested-in-data) — all three run clean, and the
labels were checked against a real play rather than written from the source.

Rule 5, three ways: with key templating off the two rendered-key tests fail; with the
indexer skip reverted the index test fails; with *every* key templated — the version this
ticket originally described — the boundary tests fail. Corpus: 753 files, two templated
`set_fact` keys, both already exempt, no new diagnostics.
