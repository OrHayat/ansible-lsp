# T-250 — The workspace walk follows symlinks back into the tree, so one file is analysed and reported under several paths

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P2       | S    | T-131 | —          |

## Symptom

`workspace::yaml_files_in` follows a symlinked directory wherever it points, including back
into the tree it is already walking. Every file under such a link is analysed again and its
diagnostics published again, under the link's spelling.

Measured 2026-10-21 on the public `debops` repo, which links `roles -> ansible/roles` and links
the same roles into its collection layout twice more: the editor's workspace scan analyses
**4,910** files for **1,399** real YAML files, and publishes diagnostics under **1,954** paths
for **604** real files — 581 of them under two to four paths, so most warnings appear three or
four times in the Problems panel. `scan` walks the same list and counts the same 4,910.

A link to an ancestor is a loop: a folder holding `play.yml` and `up -> ..` makes `scan`
report 33 files, one `play.yml` per level until the OS refuses the path (`ELOOP`).

## Cause

The first walk (2026-07-29) used `p.is_dir()`, which follows links. T-085 (`0111367`) kept
that on purpose, so a role folder symlinked from *outside* the workspace is still walked.
Neither version asked where the link lands.

## Fix

Walk each real file once, at its real path. A symlinked directory or file whose target is
inside the walked root is skipped — its target is walked under its own name. A link out of
the root is still followed, once per target, and a link back into anything already walked is
skipped, which also ends a loop.

## Done when

- [x] a link into the tree is skipped, a link out of it is walked once, a loop ends — each pinned by a test
- [x] debops: one analysis and one published path per real file, scan time measured before and after
- [x] corpus scans diffed: only the duplicate paths vanish

Measured after the fix, same machine, three runs each: the editor's debops scan went from
1,413-1,499 ms over 4,910 files to 386-428 ms over 1,389, and it publishes 604 paths for 604
real files, each with exactly the diagnostics its real path had before. No link spelling
carried a diagnostic its real path lacked. Across 44 public repos and the private corpus, 14
repos changed; in each the walk now lists exactly its real YAML files, and every finding,
mapped to real paths, is identical before and after — only the templated-variable tallies,
which counted duplicates, went down. The worst cases besides debops: keycloak 976 -> 196 files,
kubespray 1,258 -> 583.
