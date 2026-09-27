# T-230 — tags: on a dynamic include without apply: tags: reaches none of the included tasks

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P2       | S    | T-099 | —          |

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

- [x] `tags:` on `include_tasks` and on `include_role` with no `apply: tags:` each warn,
      anchored on the `tags:` key
- [x] the same line with `apply: {tags: [...]}` is silent — the control
- [x] `import_tasks` / `import_role` / a `roles:` entry with `tags:` are silent
- [x] the four edge cases above are each measured and pinned, whichever way they go
- [x] a demo fixture with GOOD / BAD rows and the exact-set test
- [x] corpus gate: every hit on `~/app/ansible` is a real one

## Landed

`include_tags.rs`; `demo/include_tags.yml` pinned exactly, with a guard over the rest of the
demo and an editor-level test for the handlers exemption. Each exemption was broken once and
its test seen red.

Measured on 2.21.3 with `--tags web`, counting which inner tasks ran:

| case                                                        | inner tasks run |
| ----------------------------------------------------------- | --------------- |
| `tags: [web]` on `include_tasks` / `include_role`           | none            |
| + `apply: {tags: [web]}` in the args                        | all             |
| + `apply: {tags: [other]}`, or `apply:` with no `tags:`     | none            |
| `tags: always` / `[never, web]` / scalar / FQCN spelling    | none            |
| tag on the enclosing `block:` or on the play                | **all**         |
| `import_tasks` / `import_role` / `roles:` entry             | all             |
| include in `handlers:`, notified                            | **all**         |
| inner tasks tagged `web` themselves                         | **all**         |

The edge cases: different `apply` tags → warn, naming only the tags that lose tasks;
`always` → warn, "under any `--tags`"; inherited block/play tags → silent, because they do
reach (not because the author didn't write them); `include_role` = `include_tasks`. Two
findings the ticket did not anticipate: handlers are exempt, and `apply:` written as a
sibling of `include_tasks:` rather than inside its args is fatal ("conflicting action
statements"). A string `tags: "web,db"` is two tags, spaces trimmed.

**Corpus gate.** The ticket's local-only design fired 116 times on the corpus, and 21 were
false: includes whose every inner task already carried the tag, a deliberate pattern there.
So the rule reads the included file — resolved during analysis like `include_targets` — and
warns only when some inner task lacks the tag, naming how many. It is silent when the target
cannot be read. Result: **94 hits**, matching an independent per-file classification (85
with no inner task tagged, 9 partly tagged) exactly. The mechanism is measured live; the 94
are judged statically from the tags written in each target.
