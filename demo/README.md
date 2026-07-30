# Demo

Everything the server does, labelled, so you don't have to hunt through 731 real files.
Open the folder in the Extension Development Host (**Run -> Start Debugging** — F5 is the
mic key on this Mac).

| File | Shows |
| ---- | ----- |
| `tasks/main.yml` | navigation — what's clickable, what deliberately isn't |
| `tasks/conditions.yml` | `when:` analysis — every verdict and every warning rule |
| `playbook.yml` | `roles:`, `import_playbook`, and `# noqa` suppression |

Prefixes are consistent: **GOOD** resolves or is analysed, **BAD** is deliberately broken,
**SILENCED** is suppressed by `# noqa`, **NO HINT** means the tool declines to answer.

## What to look for

**Teal + dotted underline** — resolves, Cmd+click jumps there. Plain text means it does not
resolve, or the kind isn't supported yet. The colour is the point: you can spot a typo before
clicking it.

**Yellow squiggle** — a literal path that resolved to nothing, or a condition that cannot
work. The message lists every path tried, in order.

**Grey text to the right of a line** — an inlay hint: what a `when:` does on a run with no
extra vars. Nothing is wrong; it's derived information. If you see none, set
`Editor > Inlay Hints: Enabled` to `on`.

Only `import_playbook` hints have a hover tooltip, because only that construct behaves in a
way the hint alone can't convey. A plain task's hint states the answer outright, so hovering
it would just restate the method.

Too chatty? Two settings, both live — no reload. They're written out in
[`.vscode/settings.json`](.vscode/settings.json) next door:

| Setting | Effect |
| ------- | ------ |
| `ansibleLsp.inlayHints.explanations` | `false` drops the hover tooltip on `import_playbook` hints |
| `ansibleLsp.inlayHints.enabled` | `false` removes the hints entirely |

**Where to actually put them.** Both are *window*-scoped, and the debug launcher opens this
folder **and** `~/matrix/ansible` as a multi-root workspace — so VS Code will not apply them
from a per-folder file there, and marks them "cannot be applied in this window". In the
Extension Development Host, set them in **User settings** (`Cmd+,`, search `ansible lsp`).
The file next door is the copy-paste source, and it does work if you open `demo/` on its own.

Window scope is deliberate: the server keeps one setting for the whole session, so letting
VS Code offer a per-folder value would promise something it cannot honour.

Neither touches diagnostics. Warnings are things you asked to be told about, so they are not
configurable here — silencing a rule is `# noqa`, or a committed project file (T-025).

## Deliberate silences

Each of these stays quiet for a reason a test pins down, and each would be a false positive
if it warned:

- **templated paths** (`"{{ protocol }}_target/check.yml"`) — a variable can expand to
  anything at runtime, so absence proves nothing. Navigation offers every candidate instead.
- **a role with no `tasks/main.yml` but a `tasks_from:`** — legal. `roles/cib-batch` in the
  real repo is exactly this and 16 working references depend on it.
- **modules from collections that aren't installed** — a missing dependency, not a typo.
- **files that fail to parse** — strict YAML 1.2 rejects files Ansible's PyYAML accepts.
  Today that means silence, which is itself a problem (T-013).
- **61% of `when:` conditions** — real boolean logic, unguarded comparisons, unknown filters.
  Guessing would make the analysis untrustworthy.

## Two traps this demo has fallen into

**Unquoted `: ` in a task name.** `name: Block form with an explicit file: parameter` is
invalid YAML, and an unparseable file yields no references and no diagnostics — so the server
goes completely silent and looks broken. It happened three times while writing these files,
and it's the whole argument for T-013. Quote any name containing a colon.

**Every example here is load-bearing.** `demo_exercises_every_problem_and_verdict` asserts
this folder still demonstrates all four warning rules and all twelve verdicts. Deleting an
example fails the build rather than quietly reducing what the demo proves.
