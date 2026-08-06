# T-034 — Templating that looks dynamic but isn't

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P2       | M    | T-120 | T-015      |

## Problem

`{{ }}` is treated as "runtime unknown, glob and never warn". That was wrong for
`role_path` — 4 real references were fully knowable and sat unnavigable until they were
expanded. The same mistake is repeated in other shapes.

Measured across all 731 files, counting only templated values in reference position:

| Pattern | Count | Knowable |
| --- | --- | --- |
| `{{ item }}` over a **literal** `loop:` | 12 | **yes, exactly** |
| `{{ item }}` over a variable `loop:` | 13 | no |
| `lookup('env', 'VAR')` | 5 | yes, controller-side |
| `\| default('literal')` | 4 | yes, when unset |

Plus the ones already handled: `role_path` (4), `playbook_dir` (4), `inventory_dir` (0).

**11 of the 12 literal-loop cases are `template:`/`copy:` `src:`**, which the extractor
does not yet cover. Hence the dependency on T-015 — building this first would resolve
almost nothing.

## The one that matters

```yaml
template:
  src: "{{ item }}.j2"
loop: ['alertmanager.container', 'vmalert.container']
```

The value is not runtime data. It is a literal list two lines below the reference. Today
this globs `*.j2`; it should resolve to exactly two files, and warn if either is absent.

`TaskContext` already reads `loop:`/`with_*` to set `repeated`, but discards the values.
Capturing them is the same change that `conditions` needed.

## Approach

Extend the expansion that `role_path` uses, rather than adding a parallel mechanism.
`expand_magic` already returns *several* candidate strings and a flag for whether any
`{{ }}` survived — that shape covers all of these:

| Source | Expands to |
| --- | --- |
| `item` + literal `loop:` | one candidate per list entry |
| `\| default('x')` | the default value |
| `ternary('a', 'b')` | both branches |
| `lookup('env', 'HOME')` | the controller's value |
| `first_found` with a literal list | each entry, first-hit-wins (already `from_candidates`) |

Rules that keep it honest:

- **Every entry must be literal.** One `{{ }}` inside the loop list and the whole thing
  stays unknown. 13 of the 25 `item` cases are exactly this.
- **A partially-expanded value is still templated** — glob it, never diagnose.
- **`vars:`/`set_fact` literals are deliberately excluded.** They look knowable but sit
  under 22 precedence levels; inventory or `-e` can override them, so expanding one would
  invent a false certainty. This is the line between "the value is in the file" and "the
  value is probably this".
- `lookup('env', …)` reads the *controller's* environment, which is right for
  `template`/`copy` `src:` and wrong for anything that runs on the managed host — so it
  needs T-015's local-vs-remote table, not just the string.

## Done when

- [ ] a literal `loop:` expands `{{ item }}` to one candidate per entry
- [ ] a variable `loop:` leaves it templated, and a test pins that
- [ ] `| default('literal')` and `ternary(a, b)` expand
- [ ] `vars:`/`set_fact` are **not** consulted, and a comment says why
- [ ] corpus gate: zero new warnings across 731 files
- [ ] `scan`'s templated-variable survey re-run; anything still unresolved is genuinely
      runtime, and this ticket records the list
