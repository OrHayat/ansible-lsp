# T-199 — Hover and go-to-definition read the saved file, so an unsaved buffer gets a wrong value and a wrong line

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P2       | M    | —          |

## Symptom

A variable is defined in one file and used in another. Open the **defining** file in the
editor, edit it, and do not save. Hover the use, or Cmd+click it, and both answer from the
file on disk — not from what is on screen.

Measured against the real code paths, with the buffer registered in `State.docs` and
`invalidate_var_cache` called exactly as `did_change` does:

| fixture           | on disk          | in the unsaved buffer     |
| ----------------- | ---------------- | ------------------------- |
| `vars/x.yml`      | `shared_port: 8080`, line 0 | `shared_port: 9999`, line 10 (ten lines prepended) |

```
BASELINE  -> vars/x.yml line 0    HOVER -> vars/x.yml:1 = `8080`
DIRTY BUF -> vars/x.yml line 0    HOVER -> vars/x.yml:1 = `8080`
```

Two separate wrong answers, not one:

1. **Hover states a value the user does not have.** It renders `` = `8080` `` while the
   screen shows `9999`. Not silence — a confident wrong number, which is the failure this
   project exists to avoid.
2. **Go-to-definition lands on the wrong line.** We return line 0. The editor opens
   `vars/x.yml` and draws the *dirty buffer*, whose line 0 is a comment. So a range computed
   against disk is applied to text rendered from memory: the two disagree, and the further
   the unsaved edit shifts things, the further off the jump. This is the half that makes it a
   P2 rather than ordinary staleness — the answer is not stale-but-coherent, it is incoherent
   with what the editor displays.

Both self-heal on save, and both need a *second* file open and edited — hence P2, not P1.
Worse than [T-132] and [T-133] (P3, "hover points somewhere imprecise"): this points
somewhere wrong.

Not a law of nature — rust-analyzer and other servers index dirty buffers.

## Cause

The open file's own text comes from the buffer: `hover` and `goto_definition` both start with
`self.state.text_of(&uri)` (`main.rs:2562`, `main.rs:2628`), and `located_at` honours it —
`main.rs:2708` uses `open_text` when the definition is in the file you are in. **Every other
file goes to disk**, through four direct `std::fs::read_to_string` calls that never consult
`State.docs`:

| site            | what it reads                                                  |
| --------------- | -------------------------------------------------------------- |
| `main.rs:2711`  | `located_at` — the target file, to turn a byte span into a line/column |
| `main.rs:1609`  | hover rendering a definition's file to show its value           |
| `main.rs:1727`  | hover rendering a definition's file to show its value           |
| `main.rs:1710`  | hover rendering a vars file                                     |

The definitions themselves are equally disk-bound: `cached_definitions` (`main.rs:194`) builds
`ScanCache::default()`, i.e. `StdFs`. The `Fs` trait is ansible-core's seam by design
(`fs.rs:1`, "the crate's one door to the filesystem") — the LSP crate never adopted it and
calls `std::fs` directly.

`did_change` does call `invalidate_var_cache(&path)` (`main.rs:2604`) so that files reading
the changed file recompute. That invalidation is inert for an unsaved buffer: the recompute
goes back to disk and produces the same answer. The intent is there, the read path cannot
deliver it.

Nothing downstream picks the buffer up either — the probe seeded `State.docs` and the answer
did not move. That is worth stating because the crate already keeps process-global state
(`var_cache`, `INVENTORY_SETTING`), so a global document registry would have been the same
shape. There isn't one.

## Fix

Give the two consumers access to the open buffers, and prefer a buffer over disk wherever a
file is read for an answer.

- `definition_at` and `variable_hover_at` do not receive `State` today. Threading it is the
  same seam [T-178]'s last-but-one box needs for its per-consumer test, so the two should be
  done together or at least not fight each other.
- The four `read_to_string` sites become "buffer if open, else disk". `located_at` already
  has the shape for this at `main.rs:2708` — it just only knows about one file.
- The *definitions* are the harder half: they come from `cached_definitions` → `StdFs`, so a
  buffer-aware answer means either an `Fs` implementation that overlays open documents, or
  passing the open text down the way the current file's `nodes` already are. The overlay is
  the one that generalises, and it is the only route that makes `MemFs` usable from this
  crate — see [T-178]'s "Why the per-consumer test is writable" for why it is not today.

Sized M for that reason: the rendering half is small, the definition half is a seam.

**Decide explicitly whether jumping into a dirty buffer should be exact or refused.** If the
overlay is too costly, the honest fallback is to return nothing rather than a line we know may
be wrong — silence beats a wrong jump target. That is a product call, not an implementation
detail, and it belongs in this ticket rather than in the diff.

## Done when

- [ ] hover on a use whose definition sits in an open, unsaved file reports the **buffer's**
      value, not the saved one — asserted per consumer, with a saved-file control that must
      still answer from disk when the file is not open
- [ ] go-to-definition returns a range valid against the **buffer**, so the jump lands on the
      definition after an edit that shifts its line — the ten-lines-prepended fixture above,
      which is the shape that made this visible
- [ ] a file open and unsaved but *unedited* still answers identically to the disk path, so
      the overlay cannot change answers it has no reason to
- [ ] seen red before the fix, per rule 5, in both directions: the current code fails the two
      boxes above, and an overlay that reads the buffer unconditionally fails the saved-file
      control
- [ ] whichever way the "exact or refused" call goes, it is written down at the site with the
      reason, not just implemented

[T-132]: T-132-go-to-definition-on-a-module-with-an-action-plugin-twin-offe.md
[T-133]: T-133-notinworkspace-hover-lumps-three-different-situations-into-o.md
[T-178]: T-178-an-inventory-source-s-ansible-group-priority-is-indexed-as-a.md
