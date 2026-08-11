# T-169 — A template inside a mapping key is invisible to the var walk

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P2       | S    | —          |

## Problem

```yaml
vars:
  result_name: my_result
tasks:
  - ansible.builtin.set_fact:
      "{{ result_name }}": true
```

Live-verified on 2.21.2: module args template their **keys** too, so this creates a fact
named `my_result` — the supported spelling for dynamic fact names (unlike the `vars:`
keyword, whose keys are static and fatal — T-103).

Our var walk sees none of it. Three faults, one root:

1. `walk_uses` (`vars.rs:300-311`) scans mapping **values** only; `k` is consulted just to
   detect `when:`. The `result_name` use inside the key gets no hover, no
   go-to-definition, and no undefined check — in `set_fact` keys and in every other
   module's arg keys alike.
2. The `set_fact` indexer (`vars.rs:394-403`) records each key **literally**, so the index
   gains a definition named `{{ result_name }}` — a name no expression can ever
   reference (braces are not valid variable-name characters).
3. The fact actually created (`my_result`) is not indexed under any name.

Today only fault 1 is user-visible (nothing can reference a brace-name, so the bogus
definition stays latent — which is why this is a task, not a bug). Fault 2 becomes
user-visible the moment anything enumerates definitions: completion (T-127), the reverse
index (T-020/T-113).

## Approach

- `walk_uses`: scan scalar mapping keys with `template_uses`, same guard accumulation as
  values. That alone fixes hover/def/undefined for every arg-key template.
- `set_fact` indexer: skip keys containing `{{` rather than recording the literal text.
  Fault 3 is knowingly left as a miss for now — resolving the defined name requires
  evaluating the template, which is T-034's statically-knowable-templating machinery;
  note it there rather than half-doing it here.
- Rule 3 applies: uses feed hover, undefined_uses and the future reverse index — a test
  per consumer, not one for the walk.

## Done when

- [ ] hover and go-to-definition work on `result_name` inside a `set_fact` key (and an
      arbitrary module-arg key)
- [ ] `undefined_uses` sees a use inside a mapping key (test with an undefined name in a
      key, and a defined one as the control)
- [ ] a templated `set_fact` key no longer produces a literal `{{ ... }}` definition in
      the index
- [ ] a literal `set_fact` key still indexes as a definition (the existing behaviour, as
      the control)
- [ ] the dynamic-name miss (fault 3) is recorded in T-034
