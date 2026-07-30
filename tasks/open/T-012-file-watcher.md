# T-012 — File watcher and precise invalidation

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P1       | L    | T-020      |

## Problem

The workspace scan runs once at `initialize` and nothing invalidates it. After that the server
is answering from a snapshot:

- rename `roles/podman` -> every dependent keeps showing **no** warning until restart
- create the missing file -> the warning stays until restart
- `git checkout` a branch with a different role layout -> everything is wrong at once

This is the worst failure mode the tool has, because a stale *absence* of a warning is worse
than never having warned. The whole value proposition is "you find out before the run," and
right now that's only true at the instant the server started. Rename-safety is precisely what
motivated a repo-wide scan in the first place.

## Why this is L, not S

The naive version is four lines — `did_change_watched_files` calling the existing
`scan_workspace()`. It was scoped S on that basis. That was wrong, for two reasons.

**The rescan is not cheap.** `scan_workspace()` is I/O bound at **~3.5 s over 731 files** (see
its doc comment). A `git checkout` fires hundreds of events, so the naive version is hundreds
of queued 3.5 s scans computing the same answer.

**The rescan only exists because the data is thrown away.** `scan_workspace()` already computes
every reference and every resolution in the workspace — the entire edge list — then drops it at
the end of each loop iteration, keeping one boolean per file in `flagged`. So "who referenced
the path that just disappeared?" requires re-deriving from scratch what was already computed and
discarded.

Building the debounce machinery to hide a 3.5 s scan means writing code whose only purpose is to
work around a `drop`. Hence the dependency on **T-020**: with the index, a delete is a lookup
returning a handful of files, and this ticket becomes what it should be — notify the referrers.

## Approach

### 1. Registration

In `initialized`, alongside the existing `scan_workspace()` call:

```rust
self.client.register_capability(vec![Registration {
    id: "watch-yaml".into(),
    method: "workspace/didChangeWatchedFiles".into(),
    register_options: Some(serde_json::to_value(
        DidChangeWatchedFilesRegistrationOptions {
            watchers: vec![
                FileSystemWatcher { glob_pattern: "**/*.{yml,yaml}".into(), kind: None },
                FileSystemWatcher { glob_pattern: "**/ansible.cfg".into(), kind: None },
            ],
        })?),
}]).await;
```

`kind: None` means create + change + delete.

### 2. Route by event type — they are not the same problem

| Event | What actually went stale | Action |
| ----- | ------------------------ | ------ |
| **Changed** | only that file's own references. No path's *existence* changed. | re-analyse that one file |
| **Created** | the existence answer for everyone who *tried* this path and failed | look up referrers, republish |
| **Deleted** | the existence answer for everyone who resolved *to* this path | look up referrers, republish |
| **`ansible.cfg`** | `roles_path`/`collections_path` moved, so every cached resolution is void | reload config, full rescan |

Changed is the common case by a wide margin — you edit constantly and create rarely — so
routing it to a single-file re-analyse removes most of the cost before any index exists.

### 3. The trap: index on candidates, not targets

A target-keyed index cannot fix the case this ticket exists for:

```yaml
include_tasks: missing.yml   # warning today
```

Nothing ever resolved to `missing.yml`, so a target-keyed index has no entry for it. You create
the file and the warning never clears — the exact bug being fixed.

The index must record **every candidate path tried**, not just the winner.
`Resolution.candidates` already carries them in order, so the data exists, but this makes the
index larger and the keying non-obvious. Most likely thing to get wrong on the first pass.

### 4. Races and event-shape problems

These are what push it to L. Each is small; there are a lot of them.

- **Open buffers win.** `scan_workspace()` already skips files with an open buffer
  (`main.rs:131`) because the buffer is newer than disk. A watcher event for an open file must
  not clobber the buffer's diagnostics. A `Changed` event for an open file is redundant with
  `didChange` and should be dropped outright.
- **Rename is delete + create, not an atomic event.** Handling the halves independently means a
  transient window where the file is "gone" and every referrer flashes a warning that clears
  milliseconds later. Diagnostics that flicker read as broken. Coalesce within the debounce
  window so a rename settles into one republish.
- **Directory deletes may arrive as one event for the directory**, not one per contained file —
  client-dependent. Deleting a role dir must invalidate everything under it, so the lookup has
  to handle a prefix, not just an exact path.
- **Overlapping scans.** If a full rescan is still needed (config change), a second one starting
  mid-publish means two writers racing on `flagged` and diagnostics that stick or vanish
  wrongly. Needs a generation counter, last-write-wins:

  ```rust
  let gen = self.rescan_gen.fetch_add(1, Ordering::SeqCst) + 1;
  // ... after the debounce sleep:
  if self.rescan_gen.load(Ordering::SeqCst) != gen { return }
  ```
- **Debounce (~300 ms)** to collapse bursts. Still wanted with the index — not to hide cost, but
  to coalesce a rename and to avoid publishing mid-`git checkout`.
- **Roots outside the workspace.** `~/ansible/roles` is on `roles_path` but deliberately
  excluded from the scan. A plain `**/*.yml` glob is workspace-relative and won't see it, so
  role files changing there stay invisible. Watching it needs a `RelativePattern` with its own
  base URI — decide explicitly whether that's in scope, and say so, rather than leaving it as an
  accident of glob semantics.

## Testing

A watcher can't be unit-tested through the filesystem reliably. Drive it at the protocol layer:
`scripts/smoke.js` already speaks raw stdio, so it can send synthetic
`workspace/didChangeWatchedFiles` notifications and assert on the `publishDiagnostics` that come
back. That covers the routing table and the races without depending on FSEvents timing.

## Done when

- [ ] renaming a role makes dependents light up without a restart, and **without flicker**
- [ ] creating a previously-missing file clears its warning — the candidate-keyed case
- [ ] deleting a role directory invalidates every reference under it
- [ ] editing `ansible.cfg` re-resolves against the new roots
- [ ] a `Changed` event for a file with an open buffer is ignored
- [ ] a burst of 200 events produces one republish, not 200
- [ ] two overlapping full rescans cannot leave a stale diagnostic behind
- [ ] `~/ansible/roles` is either watched deliberately or excluded deliberately, and this ticket
      records which
- [ ] smoke test drives all four event types over the wire
