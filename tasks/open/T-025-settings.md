# T-025 — Settings: toggle rules, override severity

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P3       | M    | —          |

## Problem

`# noqa` (T-010) is per-line only. There's no way to turn a rule off for a project, change its
severity, or see what rules exist. Once T-021/T-022/T-023 add hint-level rules, a
project-level switch stops being optional — a rule that fires 200 times can only be silenced
by editing 200 lines.

The rule ids also aren't documented anywhere, so `# noqa: missing-file` is only discoverable by
reading the source.

## Approach

Two layers, and the split matters:

**VS Code settings** — per-user, per-machine. Things about *this machine*.

Shipped: `ansibleLsp.serverPath`, `ansibleLsp.trace.server`,
`ansibleLsp.inlayHints.enabled`. It is
live — the server calls `workspace/inlayHint/refresh` on
`didChangeConfiguration`, because a setting that only takes effect on the next keystroke
reads as a setting that doesn't work.

Still wanted: `ansibleLsp.scanOnStartup`.

**A setting for an invisible feature is worse than no setting.** There was briefly a second
switch for inlay-hint tooltips — named `explanations`, then `tooltips`. Both readings failed:
turning it off left the hints in place, so it looked broken. The root cause wasn't the name,
it was that VS Code shows an inlay hint's tooltip only when you hover the hint label itself,
a ~10px target nobody discovers. The information moved inline and the setting was deleted.
Retired keys are covered by a test asserting they can't disable anything.

The server logs its effective settings at startup because "the setting does nothing" is
otherwise unfalsifiable from inside the editor.

**The line that matters:** only *volunteered* output is configurable per-machine.
Diagnostics are not — a warning is something you asked to be told about, and whether a rule
runs is a property of the repo, not of who opened it. That belongs in the project file
below, committed, so CI and every editor agree.

**A project file** — `.ansible-lsp.toml` next to `ansible.cfg`, committed: which rules are on,
severity overrides, extra roles paths. Things about *this repo*, which have to travel with it,
work in `bin/scan.rs` (no editor, no VS Code settings), and be the same for everyone.

```toml
[rules]
missing-file     = "warning"
templated-import = "warning"
unused-file      = "off"
shadowed-file    = "hint"
```

`off` / `hint` / `info` / `warning` / `error`. Absent = the rule's default.

Also: document every rule id and its default in one table. Currently `missing-file` and
`templated-import`; T-013/T-021/T-022/T-023 each add one. Without the table, `# noqa` is
guesswork.

## Done when

- [ ] `.ansible-lsp.toml` disables a rule and changes a severity
- [ ] `bin/scan.rs` honours the same file, so CI and editor agree
- [ ] every rule id documented with its default
- [ ] absent file behaves exactly as today
