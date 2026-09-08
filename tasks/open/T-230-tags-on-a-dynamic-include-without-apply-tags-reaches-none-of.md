# T-230 — tags: on a dynamic include without apply: tags: reaches none of the included tasks

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | S    | T-099 | —          |

## Problem

`tags:` on an `include_tasks` / `include_role` line tags the include task itself and
nothing it brings in. Under `--tags x` the include runs and every task inside is skipped,
which is the opposite of what the author meant by writing the tag there. Static imports
inherit keywords; dynamic includes need `apply: {tags: [...]}` to reach through, and the
module doc says so ("tags ... are not automatically inherited by the include tasks, see
apply"). The editor is silent on it today.

Live-verified in T-063 on 2.21.2: `tags: [outer]` on an `include_role` plus `--tags outer`
runs **one** task, the include, and nothing inside the role; adding `apply: {tags: [outer]}`
runs all three. T-063 noted it as a lint candidate and gave it no box — this is that box.

## Approach

A warning on the `tags:` key of a dynamic include whose `apply:` carries no `tags:`. Purely
local: the include line is the whole input, so no walk and no dependency.

Cases to settle by measurement before the message is written:

- `apply: {tags: [...]}` present with a *different* set than the outer `tags:` — the outer
  tag still selects the include line only. Decide whether that is silence or a note; the
  honest message names both sets.
- `tags: always` on the include — the include always runs, but its tasks still do not
  inherit. Same warning, or does `always` make the author's intent ambiguous?
- a `block:` or a play-level `tags:` enclosing the include — inherited onto the include
  line, so the same non-propagation applies. Measure whether that fires the warning
  (probably not: the author did not write the tag on the include) and say why.
- `include_role` vs `include_tasks` behave the same here — confirm with the control.

Message states what happens, not what to do: "`--tags` selects this include line only; the
tasks it brings in are not tagged. `apply: {tags: [...]}` is how a tag reaches them."
Suppressible per line via `noqa`.

## Done when

- [ ] `tags:` on `include_tasks` and on `include_role` with no `apply: tags:` each warn,
      anchored on the `tags:` key
- [ ] the same line with `apply: {tags: [...]}` is silent — the control
- [ ] `import_tasks` / `import_role` / a `roles:` entry with `tags:` are silent
- [ ] the four edge cases above are each measured and pinned, whichever way they go
- [ ] a demo fixture with GOOD / BAD rows and the exact-set test
- [ ] corpus gate: every hit on `~/app/ansible` is a real one
