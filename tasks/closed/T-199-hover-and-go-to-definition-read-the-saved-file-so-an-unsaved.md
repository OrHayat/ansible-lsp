# T-199 — Hover and go-to-definition read the saved file, so an unsaved buffer gets a wrong value and a wrong line

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | bug  | P2       | M    | —          |

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

Landed as an overlay, so **exact** — the jump goes to the buffer's line rather than refusing.
Refusing would have been the honest fallback only if the buffers could not reach the
definitions, and they can: `Fs` is already ansible-core's one door to the filesystem
(`fs.rs:1`), so `OverlayFs` overrides `read` alone and every consumer gets the same text. The
reasoning sits on `OpenDocs` in `main.rs`, at the site.

The buffers travel as a value, `OpenDocs` — a path-keyed snapshot taken once per request —
not as `&State`:

- `State` is the server's lifecycle object (client-tracked diagnostics, the scan flag,
  workspace roots). The readers are rendering functions; handing them `State` puts the scan
  flag in scope of a markdown renderer, and forces 30 unrelated tests to build a server.
- It cannot cross the crate boundary. The definitions are built inside ansible-core behind
  `Fs`, which knows nothing about the LSP crate. `OpenDocs` wraps into `OverlayFs`; `State`
  could not.
- Its `Default` is a claim, not a filler: empty means "no editor behind this". Tests spell it
  `no_buffers()`.
- A snapshot cannot shift under one answer, so a hover can't report a value read before an
  edit against a line number read after it.

`State.docs` stays the only storage — `State::open_docs()` derives the snapshot per request,
dropping `untitled:` URLs, which have no path for another file to name. A first attempt used a
process-global registry beside `State.docs`; two lists of open documents that must be kept in
step is the drift this project files bugs about, and it was dropped.

Every reader of the variable index was enumerated (rule 3) and each passes what it has:

| consumer                          | gets                                    |
| --------------------------------- | --------------------------------------- |
| `variable_hover_at`               | threaded from the `hover` handler       |
| `variable_defs_at` / `definition_at` | threaded from `goto_definition`      |
| `path_substitution_hover`         | threaded from `hover_at`                |
| `variable_coverage_diagnostics`   | `Analysis::open`, so the diagnostic and the hover cannot contradict each other over one variable |
| `resolved_references`             | a snapshot, so what is painted clickable matches what go-to-definition answers |
| the workspace scan                | one snapshot for the pass — it skips *open* files, but a closed file it analyses may read an open one |

The `ScanCache` inside `analyze_text_in` stays disk-backed on purpose: it answers "what does
the tree look like", which an unsaved edit to a file's contents does not change. Only the
variable index reads text for values, and `cached_definitions` builds its own overlay-backed
cache.

## Done when

- [x] hover on a use whose definition sits in an open, unsaved file reports the **buffer's**
      value, not the saved one — asserted per consumer, with a saved-file control that must
      still answer from disk when the file is not open
- [x] go-to-definition returns a range valid against the **buffer**, so the jump lands on the
      definition after an edit that shifts its line — the ten-lines-prepended fixture above,
      which is the shape that made this visible
- [x] a file open and unsaved but *unedited* still answers identically to the disk path, so
      the overlay cannot change answers it has no reason to
- [x] seen red before the fix, per rule 5, in both directions: the current code fails the two
      boxes above, and an overlay that reads the buffer unconditionally fails the saved-file
      control
- [x] whichever way the "exact or refused" call goes, it is written down at the site with the
      reason, not just implemented

[T-132]: T-132-go-to-definition-on-a-module-with-an-action-plugin-twin-offe.md
[T-133]: T-133-notinworkspace-hover-lumps-three-different-situations-into-o.md
[T-178]: T-178-an-inventory-source-s-ansible-group-priority-is-indexed-as-a.md

## What landed

Three tests in `crates/ansible-lsp/src/main.rs`, one fixture: `shared_port` defined in
`vars/x.yml`, used from `play.yml`, so every answer has to read a *second* file.

| test | asserts |
| ---- | ------- |
| `hover_of_a_cross_file_use_reads_the_open_buffer_not_the_saved_file` | `9999` and `x.yml:11`, never `8080` |
| `go_to_definition_returns_a_range_valid_against_the_open_buffer` | range starts at line 10, not 0 |
| `an_open_but_unedited_buffer_answers_exactly_like_the_saved_file` | the overlay is inert where it has nothing to add |

Both directions of rule 5 were run, and both mutations were confirmed present in the file
before believing the result:

- **Buffers ignored** (`OpenDocs::read` → straight to disk): the two tests fail on `8080` and
  line 0. The saved-file controls still pass, which is what says they are testing the
  *preference* and not just the plumbing.
- **A buffer handed to the control**: the "a closed file answers from disk" assertions fail
  (line 10 where 0 is required). The controls discriminate; they are not decoration.
