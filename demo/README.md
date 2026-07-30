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

Only `import_playbook` hints have a hover tooltip, and it carries a per-site fact rather than
a lecture — how much the condition actually covers:

```
runs unless daos_deployment_mode changes from native
  ⤷ Copied onto 15 tasks across 5 plays, evaluated separately at each — not a single gate.
```

The hint says what the condition decides; the tooltip says how much it decides. An earlier
version was a paragraph about pushed-down semantics — identical on all 48 sites in the real
repo, so it stopped being read. A count differs every time.

Plain task hints have no tooltip: the hint already states the answer, so hovering could only
restate it.

One switch, applied immediately: **`ansibleLsp.inlayHints.enabled`** — `false` removes the
hints. It's in [`.vscode/settings.json`](.vscode/settings.json) next door. Diagnostics are
unaffected; a warning is silenced per-line with `# noqa: <rule-id>`.

Being *window*-scoped, it will not apply from a per-folder file in the multi-root debug
window — set it in User settings (`Cmd+,`, search `ansible lsp`). If a change seems to do
nothing, View → Output → **Ansible LSP** logs what the server actually received:

```
ansible-lsp ready — initializationOptions: {"inlayHints":{"enabled":false}} |
  effective: inlayHints.enabled=false
```

`initializationOptions: none` means the dev host is running an old `extension.js` — Run →
Stop Debugging, then start again.

An `import_playbook` hint also carries how much its condition covers, inline:

```
runs unless daos_deployment_mode changes from native · copied onto 15 tasks in 5 plays
```

The first half says what the condition decides, the second how much it decides. That count
used to be a hover tooltip, which meant mousing over a ~10px grey label — so nobody ever saw
it. **There are no tooltips now.** A hint you have to discover by hovering is a hint that
does not exist.

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
