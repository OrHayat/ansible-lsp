# T-170 — A template in a mapping key that is never rendered

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P2       | S    | —          |

## Problem

Found while measuring T-169, which asked which mapping keys Ansible templates. Answer:
almost none. `set_fact` and `set_stats`' `data:` render a key — upstream calls it "a rare
case where key templating is allowed" (`action/set_fact.py:44`) — and every other spelling
fails, in one of two ways. Measured on 2.21.2:

| spelling                                       | what happens                                        | tier    |
| ---------------------------------------------- | --------------------------------------------------- | ------- |
| `debug: {"{{ argname }}": x}`                  | fatal: `Unsupported parameters … {{ argname }}`      | error   |
| `add_host: {name: h, "{{ k }}": v}`            | runs; host var is literally named `{{ k }}`          | warning |

Both are the author reaching for a dynamic name and getting nothing. The second is the
worse one: `add_host` accepts arbitrary keys as host vars, so it looks exactly like a third
rendering site, and it neither errors nor warns. The intended variable is simply never set,
and the one that *is* set cannot be referenced from any expression. Verified by reading the
host's own vars back in a later play: `['{{ k }}']`.

This is the same shape as T-103 — a field where braces are text, split by severity into
"Ansible refuses to run" and "Ansible runs and misbehaves" — but a different mechanism:
T-103's four keywords are declared `static=True` upstream, while these are ordinary args
whose *keys* simply never reach the templar. And it is not T-168 either: that one is the
**unquoted** spelling, which dies in the YAML loader before any of this.

Today we say nothing about either.

## Approach

- The error half is a lookup we already have: if a key carries `{{` and the action is not
  one of the two rendering sites, the module rejects it. The risk is claiming this for a
  module that legitimately takes free-form keys, so the rendering sites and `add_host` must
  come from an enumerated list, not from a guess about the module.
- The warning half is `add_host` specifically. `group_by` takes a `key:` and is worth
  measuring beside it before writing either message.
- Restrict to **module arg keys**, at the top level of the args mapping. A key nested
  inside a value is ordinary data, where braces in a key are legal and mean nothing —
  measured, and already the boundary T-169's walk draws.
- Rule 2: the control that must come out different is `set_fact`/`set_stats`, which are
  correct and must stay silent. T-169's tests already pin them from the other direction.

## Done when

- [ ] a templated key on an ordinary module's args is an error naming the real failure
      (`Unsupported parameters`), not a generic templating remark
- [ ] a templated key on `add_host` is a warning saying the host var takes the braces
      literally and the intended name is never set
- [ ] `set_fact` and `set_stats`' `data:` keys stay silent — the control, asserted
- [ ] a templated key nested inside a value stays silent, at every depth
- [ ] `group_by` measured and either covered or recorded as out of scope with the result
- [ ] demo rows for both tiers, and a corpus gate over the 39 templated keys in the tree
      (2 are `set_fact`, so the rest are the population this rule judges)
