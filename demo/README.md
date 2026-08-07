# Demo

Everything the server does, labelled, so you don't have to hunt through hundreds of real files.
Open the folder in the Extension Development Host (**Run -> Start Debugging** — F5 is the
mic key on this Mac).

| File | Shows |
| ---- | ----- |
| `ansible.cfg` | marks the project root, so `playbook_dir` and the role path have values |
| `tasks/main.yml` | navigation — what's clickable, what deliberately isn't |
| `tasks/conditions.yml` | `when:` analysis — every verdict and every warning rule |
| `playbook.yml` | `roles:`, `import_playbook`, and `# noqa` suppression |
| `tasks/role_entrypoints.yml` | which file a role entry point loads — `.yml`/`.yaml`/`.json`/no extension, and how `tasks_from:` flips the order (T-091) |
| `tasks/lenient_scalar.yml` | valid to Ansible but rejected by strict YAML 1.2 — parses since the libyaml swap (T-036) |
| `tasks/unparseable.yml` | genuinely invalid YAML (broken for Ansible too) — the `unparseable` hint, not silence |
| `tasks/unparseable_silenced.yml` | the same break, quieted with `# noqa: unparseable` |
| `duplicate_keys.yml` | duplicate mapping keys at play level, in `vars:` and in a task — valid YAML, first value silently discarded (T-102, not yet flagged) |
| `plays/duplicate_keys_json.yml` | the same mistake in JSON, where Ansible's own check never runs |

Prefixes are consistent: **GOOD** resolves or is analysed, **BAD** is deliberately broken,
**SILENCED** is suppressed by `# noqa`, **NO HINT** means the tool declines to answer.

## What to look for

**Teal + dotted underline** — resolves, Cmd+click jumps there. Plain text means it does not
resolve, or the kind isn't supported yet. The colour is the point: you can spot a typo before
clicking it.

**Yellow squiggle** — a literal path that resolved to nothing, or a condition that cannot
work. The message lists every path tried, in order.

**Hover a `when:` reference** — hover the value of a conditional `import_playbook` (or any
reference carrying a `when:`) to see what the condition does: every clause spelled out, and,
for an import, the note that the condition is copied onto every task in the imported file.
It's on demand, so there's no grey inline text cluttering the line or fighting the editor's
own end-of-line blame.

**Red error, `unparseable`** — a file that isn't valid YAML (see `tasks/unparseable.yml`).
The parser matches Ansible's (libyaml), so a file we can't parse is one Ansible can't load
either — a play that includes it will fail. It's flagged on the offending line rather than
silently skipped. `# noqa: unparseable` silences it for the templated/partial files you know
won't parse standalone.

One switch, applied immediately: **`ansibleLsp.inlayHints.enabled`** — `false` removes the
`when:` hover. It's in [`.vscode/settings.json`](.vscode/settings.json) next door. Diagnostics
are unaffected; a warning is silenced per-line with `# noqa: <rule-id>`.

Being *window*-scoped, it will not apply from a per-folder file in the multi-root debug
window — set it in User settings (`Cmd+,`, search `ansible lsp`). If a change seems to do
nothing, View → Output → **Ansible LSP** logs what the server actually received:

```
ansible-lsp ready — initializationOptions: {"inlayHints":{"enabled":false}} |
  effective: inlayHints.enabled=false
```

`initializationOptions: none` means the dev host is running an old `extension.js` — Run →
Stop Debugging, then start again.

## Deliberate silences

Each of these stays quiet for a reason a test pins down, and each would be a false positive
if it warned:

- **templated paths** (`"{{ protocol }}_target/check.yml"`) — a variable can expand to
  anything at runtime, so absence proves nothing. Navigation offers every candidate instead.
  **But not every `{{ }}` is unknowable**: `playbook_dir` is a magic variable whose
  candidate values we hold, so it expands to literal paths that get checked. (`role_path`
  and `inventory_dir` were expanded too — disabled as unsound: the first is the invoking
  role's dir, not the folder's, until T-068; the second is per-host from inventory
  sources, until T-070.)
- **a role with no `tasks/main.yml` but a `tasks_from:`** — legal. `roles/cib-batch` in the
  real repo is exactly this and real working references depend on it.
- **a shadowed role file** (`roles/entrypoints/tasks/main.yaml`, dead because `main.yml` is
  probed first) — Ansible says nothing and neither do we. The file is real and loads fine on
  its own; that it is unreachable *here* is T-023's hint to draw, not a missing-file error.
- **modules from collections that aren't installed** — a missing dependency, not a typo.
- **61% of `when:` conditions** — real boolean logic, unguarded comparisons, unknown filters.
  Guessing would make the analysis untrustworthy.

## Two traps this demo has fallen into

**Unquoted `: ` in a task name.** `name: Block form with an explicit file: parameter` is
invalid YAML — for Ansible too, so the play won't run. The server used to go silent on such
files (looking broken itself); T-013 made it flag them, and since the parser now matches
Ansible it's a hard `unparseable` error. It happened three times while writing these files.
Quote any name containing a colon.

**Every example here is load-bearing.** `demo_exercises_every_problem_and_verdict` asserts
this folder still demonstrates all four warning rules and all twelve verdicts. Deleting an
example fails the build rather than quietly reducing what the demo proves.
