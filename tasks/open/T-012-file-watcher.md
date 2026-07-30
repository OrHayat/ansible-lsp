# T-012 — File watcher: diagnostics go stale

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P1       | S    | —          |

## Problem

The workspace scan runs once, at `initialize`. After that nothing invalidates it, so:

- rename `roles/podman` -> every dependent keeps showing **no** warning until restart
- create the missing file -> the warning stays until restart
- delete a task file -> nothing lights up

This is the worst failure mode the tool has. A stale *absence* of a warning is worse than
never having warned: the whole value proposition is "you'll know before the run," and right
now the answer is only correct at the moment the server started. Rename-safety is precisely
the case that motivated the repo-wide scan in the first place.

## Approach

`workspace/didChangeWatchedFiles` — register `**/*.yml`, `**/*.yaml`, `**/*.cfg` at
initialize. The client already has `vscode-languageclient`, so registration is a few lines.

Invalidate rather than rescan: the resolution cache is keyed by (search root, relative path),
so a create/delete drops the entries whose *key* could match the changed path, and republishes
diagnostics for files whose references pointed there. That needs T-020's reverse index to be
precise about which files to republish — without it, fall back to rescanning the workspace,
which the scan timing says is ~60–100 ms and therefore fine as a first cut.

`ansible.cfg` changing is different in kind: it moves the search roots, so everything is
invalid. Rebuild the config and rescan.

Note the scan already excludes `~/ansible/roles` (outside the workspace) — it must not start
watching it either, or every unrelated edit there triggers work.

## Done when

- [ ] renaming a role in `~/matrix/ansible` makes dependents light up without a restart
- [ ] creating the missing file clears its warning without a restart
- [ ] editing `ansible.cfg` re-resolves against the new roots
- [ ] no watcher registered outside the workspace
