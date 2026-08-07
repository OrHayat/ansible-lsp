# T-023 — `shadowed-file` / `duplicate-role` hints

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P3       | M    | T-120 | —          |

## Problem

Two ambiguities that Ansible resolves **silently**, so today they are invisible until
something runs the wrong file.

**`shadowed-file`.** When `include_tasks: dup.yml` could match in more than one search root,
Ansible takes the first and says nothing. Verified empirically against real `ansible-playbook`
and pinned in `tests/fixtures/ambiguous/` — the role's `tasks/` dir beats the including file's
own directory, which is the opposite of what most people assume. The resolver already computes
the full candidate list, so it already knows when there was more than one hit; it just discards
that fact.

**`duplicate-role`.** The same role listed twice in one `roles:` block, or reached both
directly and via a `meta` dependency. Ansible runs it twice unless `allow_duplicates: false`
is set in the role's `meta/main.yml` — which is the default, so usually it runs *once* and the
second entry is a silent no-op. Either way the author's intent is unclear.

Pinned against ansible-core 2.21.2:

```yaml
# playbook.yml
- hosts: webservers
  roles:
    - foo
    - foo

# roles/foo/meta/main.yml
allow_duplicates: true
```

`allow_duplicates: true` → the role's tasks run twice (`ok=2`); `false` → once (`ok=1`).
Ansible prints **nothing** either way, at any verbosity.

Note this is a sequence with a repeated item, not a duplicate mapping key — so
`DUPLICATE_YAML_DICT_KEY` never fires here and T-102 does not cover it. Both entries survive
parsing intact; nothing is shadowed at the YAML level. That is what makes the message hard to
word: unlike a duplicate key, there is no discarded value to point at, and with
`allow_duplicates: true` the repetition is the documented way to run a parameterised role
more than once.

## Approach

Both are HINT severity, both anchored at the reference. Neither is a bug on its own — the code
works, it just doesn't say what it does.

`shadowed-file`: message names the file that won and the ones that lost, in order. That's the
whole value — the resolution order is unintuitive, so showing it teaches it.

`duplicate-role`: needs `allow_duplicates` read from the target role's `meta/main.yml` to word
the message correctly. Saying "runs twice" when the role sets `allow_duplicates: false` would
be wrong, and a hint that states a falsehood is worse than no hint.

Prevalence unknown for both. Worth measuring with `scan` before building the diagnostics — if
`shadowed-file` fires 200 times the design needs to change, and that's cheap to find out.

## Done when

- [ ] `scan` reports counts for both, so prevalence is known before the UI exists
- [ ] `shadowed-file` names winner and losers in resolution order
- [ ] `duplicate-role` reads `allow_duplicates` and words the message accordingly
- [ ] the `ambiguous/` fixture reports `shadowed-file` exactly once
