# T-162 — The parser drops scalar style, so no rule can tell 42 from "42"

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P2       | M    | —          |

## Problem

`Node::Scalar { value: String, span }` (`parse.rs:43`) keeps the text and nothing else. libyaml
hands us more — every `EventData::Scalar` carries `style` (plain, single-quoted, double-quoted,
literal, folded) and `plain_implicit` — and `build` discards it.

So `42` and `"42"` are the same node. To ansible they are an **int** and a **str**, and several
of its rules turn on exactly that difference. Each one is a documented miss on our side today,
and each miss is recorded separately because there was no single place to record the cause:

| rule | fatal spelling | legal spelling | note in code |
| ---- | -------------- | -------------- | ------------ |
| T-110 row 17 | `hosts: 42` | `hosts: "42"` | `placement.rs`, "no scalar style" |
| T-110 row 28 | `tags: 42` | `tags: "42"` | `tags_checks` |
| T-110 row 30 | `tags: [7]` | `tags: ["7"]` | blocked entirely — see below |

Row 30's int half is the one that turned a nuisance into a ticket. An int tag is declared legal
by `listof=(str, int)`, runs clean, **cannot be selected by `--tags`**, and crashes both
`--list-tasks` and `--list-tags` (`upstream/ansible-tags-member-types.md`, issues 1-2). It is
worth a warning, and a warning is impossible: firing it would hit `tags: ["7"]`, which is legal
and works. A false error on correct code is the one thing this codebase does not do, so the
rule stays unwritten rather than approximate.

The shape half of row 30 shipped without this — a list or mapping member is a *node kind*, not
a style — which is what makes the boundary clear: everything that needs the **type** of a
scalar is stuck here, and nothing else is.

Expect more of these as T-108/T-109 land. Every `isa='int'`, `isa='bool'` and `isa='float'`
coercion upstream has the same shape: `port: "22"` and `port: 22` differ, and `become: "yes"`
is not `become: yes` in every version.

## Approach

Carry the style on the node and let the rules ask a question in ansible's terms, not YAML's.

- Add `style` to `Node::Scalar`, populated from `EventData::Scalar`. It is already in the event
  we throw away, so this is plumbing, not new parsing.
- The rules should not match on `style` directly. What they want is "what type would the YAML
  loader have produced" — so expose that as a method (`Node::implicit_type()` or similar)
  returning str / int / float / bool / null, and resolve it the way YAML 1.1 core schema does:
  a **plain** scalar matching the int/float/bool patterns is that type, and any quoted or block
  scalar is always a string. Rules then read `implicit_type`, and none of them learns what a
  single-quoted scalar is.
- **`ansible` uses YAML 1.1**, not 1.2, via PyYAML — so `yes`, `no`, `on`, `off`, `y`, `n` are
  booleans and `0o17`/`017` octal rules differ. Do not port a 1.2 table. Measure the edges
  against the installed core before encoding them; `become: y` is the kind of case that decides
  whether this is right.
- Sexagesimals (`1:30` is 90 in YAML 1.1) are the trap everyone hits. Confirm whether PyYAML's
  resolver still has them and match whatever it does, including if that is "no".
- The struct grows for every scalar in every file, so measure the parse-and-scan cost on the
  workspace before and after. A one-byte enum should be free; confirm rather than assume.

## Done when

- [ ] `Node::Scalar` carries the style libyaml already reported, and a round-trip test pins each
      of the five styles
- [ ] `implicit_type()` returns what PyYAML's resolver would, measured against the installed
      ansible-core rather than ported from a spec — including the YAML 1.1 booleans
- [ ] T-110 row 17 fires on `hosts: 42` and stays silent on `hosts: "42"`, both asserted
- [ ] T-110 row 28 does the same for `tags: 42`
- [ ] T-110 row 30's int half lands as a WARNING — the play runs, so it is not an error — naming
      what breaks: `--tags` will not select it and `--list-tasks` crashes
- [ ] the three "no scalar style" miss notes in `placement.rs` are deleted, not reworded
- [ ] parse+scan wall clock on the demo workspace is unchanged within noise
