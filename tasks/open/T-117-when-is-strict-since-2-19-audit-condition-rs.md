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
| `when: ""` or `when: null` | **fatal** — "Empty conditional expressions are not allowed." | `:490-514` |
| a non-boolean result | **fatal** — "Conditionals must have a boolean result." | `:562-591` |
| `when: "{{ x }}"` fully wrapped | resolved once then evaluated; **deprecated** for 2.23 | `:534-546` |
| `when: x == '{{ y }}'` partial embedding | gated on `ALLOW_EMBEDDED_TEMPLATES` | `config/base.yml:78-85` |

`ALLOW_BROKEN_CONDITIONALS` defaults `false` (`config/base.yml:63-77`) and is itself slated
for removal in 2.23, so the strict behaviour is the only one worth modelling going forward.

The fully-wrapped and partially-embedded cases are **pure syntax** — no variable index, no
evaluation — so they are cheap additions to a file that already parses conditions.

This is P1 not because it adds coverage but because it is a correctness audit of rules that
already ship. T-032 is the ticket that built them; this is the version check they never had.

## Approach

Walk the four existing rules against the 2.19+ semantics, then add the two syntactic cases.
Gate anything version-sensitive on the detected ansible-core version — **which does not
exist yet**. `AnsibleInstall` records where Ansible is, not which version it is; T-138 adds
the field by reading `<package_dir>/release.py`. Do that first or this ticket has nothing to
gate on.

## Done when

- [ ] each existing `when-*` rule is confirmed against 2.19+ or corrected
- [ ] `when: ""` and `when: null` are ERRORs
- [ ] a fully-wrapped `when: "{{ x }}"` is a deprecation HINT
- [ ] version-sensitive rules are gated on the detected ansible-core version
- [ ] the corpus re-run still finds zero broken conditions, or explains what changed
