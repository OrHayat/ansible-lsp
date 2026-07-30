# T-031 — `import_playbook` + `when:`: say what it actually does

| Status          | Priority | Size | Depends on |
| --------------- | -------- | ---- | ---------- |
| **partly done** | P2       | M    | T-029      |

## Problem

**48 of the 73 `import_playbook` references carry a `when:`** — two thirds of them. And the
construct does not mean what it reads like.

```yaml
- name: Deploy in native mode
  import_playbook: lustre-native.yml
  when: lustre_deployment_mode | default('native') == 'native'
```

That reads as a gate: *run this playbook if the mode is native.* It isn't. A static import is
expanded at parse time, before any variable exists, so there is nothing to gate. Ansible instead
**copies the condition onto every task in every play of the imported playbook**, where it is
evaluated per-task, in each task's own variable scope.

Consequences that surprise people:

- the plays still run — hosts are matched, the play is set up, handlers are registered. Only the
  *tasks* are individually skipped.
- because the condition is re-evaluated per task, anything that changes the variable mid-playbook
  (a `set_fact`, a registered result) changes the condition **partway through**. A gate can't do
  that; this can.
- the condition is duplicated N times, so a typo'd variable name fails N times, not once.

Nothing in the toolchain says any of this. `ansible-lint` doesn't, and the semantics aren't
obvious from the YAML.

This was flagged in T-009 as *captured but unused* — `Reference.conditional` is already
populated and nothing reads it. This ticket is where it gets used.

## What was already ruled out

An earlier idea — flag *risky* conditions on imports, the ones that could blow up on an
undefined variable — died on the data. Every condition in the repo already guards itself:

| Pattern                              | Count | % of the 56 |
| ------------------------------------ | ----- | ----------- |
| `not (X \| default(false) \| bool)`  | 45    | 80%         |
| `X \| default('v') == 'lit'`         | 5     | 8%          |
| `X is defined`                       | 2     | 3%          |
| `X \| length`                        | 2     | 3%          |
| other                                | 4     | 7%          |

Nothing to flag. A "risky condition" rule would have been a pure false-positive generator. The
useful thing isn't finding bugs here — it's **explaining the construct**.

## Approach — revised, and partly shipped

**The diagnostic was dropped in favour of an inlay hint** (shipped, see T-032). 48 permanent
INFORMATION rows in the Problems panel would have been suppressed wholesale within a day, and
then T-024's labelling would have lost its explanation too. The hint carries the verdict
inline; its tooltip carries the pushed-down explanation on hover only.

What remains open here is the *explanation surface*, not the detection:

- a fuller hover (T-029) covering the per-task copy and the fact-gathering cost
- a code action offering `meta: end_play` or extraction to a task file
- the empirical check below, which decides how strongly the wording can be put

Message has to be short and specific about the mechanism, and about the actual cost:

> `when:` on a static import is copied onto every task in the imported playbook and evaluated per
> task — the plays still run and facts are still gathered. For a single gate, use
> `meta: end_play` inside the imported playbook.

Three things this must not do:

- **never say "conditional"** for this. That's the framing error that killed T-011's tree. The
  word is *pushed down*.
- **never recommend `include_playbook`. It does not exist.** Ansible has `include_tasks`/
  `import_tasks` and `include_role`/`import_role`, but playbook level has only
  `import_playbook` — there is no dynamic variant. `when:` is the *only* conditional mechanism
  available at that level, which is why all 48 sites use it. The alternatives are
  `meta: end_play` inside the imported playbook, restructuring into a task file reached by
  `include_tasks`, or tags with `--skip-tags`.
- **never suggest the code is wrong.** 48 sites, all deliberate, and all conditioning on
  inventory or `-e` variables that cannot change mid-run — so the per-task copy is observably
  equivalent to a gate here. An info that reads as a reprimand gets suppressed wholesale, and
  then T-024's labelling loses its explanation too.

### What the cost actually is

The copy differs from a real gate only when the variable can change during the run — a
`set_fact` or a registered result. None of the 48 do that. What remains is overhead, not
incorrectness:

- the play is set up and its banner prints, so a "skipped" playbook still looks like it ran
- facts are gathered for those hosts regardless
- every task reports `skipping:`, so a skipped playbook emits N lines of output

**Verify this before writing the message.** Whether implicit fact-gathering is actually
suppressed is the kind of thing this project has been wrong about before — `first match wins`
and the char-offset markers were both settled by running the real tool, not by reading docs.
Build a two-playbook fixture, run `ansible-playbook`, and record the result in the board's
*Settled* table. It decides whether the message says "costs you fact-gathering" or something
stronger.

Feeds T-024 directly: the tree's **pushed down** label is the same fact rendered differently, and
both should come from one place in the code.

## Done when

- [x] all 48 conditional imports are annotated — as inlay hints, not diagnostics
- [x] the tooltip explains the per-task copying, not just "this is conditional"
- [x] the word *conditional* appears nowhere in the wording
- [x] `demo/playbook.yml` exercises the case and its comments agree
- [ ] fact-gathering behaviour verified against real `ansible-playbook`, result recorded in
      the board's *Settled* table
- [ ] a code action offering `meta: end_play` or extraction to a task file
- [ ] the explanation lives in one function shared with T-024's label
