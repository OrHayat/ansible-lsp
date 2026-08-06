# T-081 — The board is hand-edited, and it has drifted

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | S    | —          |

## Problem

`tasks/README.md` says **"Status is the folder"** — `ls tasks/open` is the real backlog. But the
tables that everyone actually reads are maintained by hand, and they no longer agree with the
folders. Measured 2026-08-04:

| disagreement                              | count | examples                                    |
| ----------------------------------------- | ----- | ------------------------------------------- |
| listed as open, file is in `closed/`      | 2     | T-018, T-033                                |
| file in `open/`, no README row at all     | 14    | T-046, T-051, T-054, T-062, T-064, T-072, … |
| file in `closed/`, no README row          | 11    | T-044…T-056, T-066                          |

48 files in `open/` against 36 open rows; 31 in `closed/` against 18 closed rows.

**This misdirects work, and it already did.** Asked for a small next task, the obvious move is to
read the P-tables and pick an `S`. T-018 sits in the P2 table at size `S` and has been closed for
some time — it was one of the three shortlisted before the folder contradicted the table. The 14
unlisted open tickets are the worse half: T-062, T-064 and T-072 are all cited as blockers *inside*
other tickets (T-022 refers to T-062 twice), yet none of them appear on the board. Choosing from
the README means choosing from 36 of 48 tickets, two of which are done.

Every close is three manual edits — `git mv`, flip the status line, move the README row — and the
third is the one with no failure mode. Nothing breaks when it's skipped, which is why 25 of them
were.

## Approach

Generate the Open and Closed tables from the ticket files themselves. Every ticket already opens
with its own status/priority/size table, so the data exists in exactly one place; the README's
copy is the duplicate. Keep the hand-written prose — "Where the project actually is", "Settled —
don't re-derive these", the per-section notes — that's the part with judgement in it and it must
not be clobbered by a generator.

Two forms, in increasing cost:

- **check only.** A script that exits non-zero when the folders and the tables disagree, listing
  each difference. Catches all 27 above, writes nothing, no risk of eating prose. Wire it into
  whatever runs the tests.
- **generate.** The same script rewrites the table blocks between markers, leaving prose alone.
  Then closing a ticket is `git mv` plus a regen.

Start with the check — it is the part that has to exist either way, since a generator with no
check just silently produces whatever it produces.

Priority grouping (P1/P2/P3) and the `Blocked by` / `Refs` columns live in the ticket headers
today only sometimes; the check will surface which files are missing them. That inventory is
part of the ticket, not a prerequisite for it.

## Done when

- [x] a script reports every disagreement between `tasks/{open,closed}/` and the README tables,
      and exits non-zero when there is one
- [x] the 27 existing disagreements are fixed, so the check passes on a clean tree
- [x] closing a ticket no longer requires hand-editing a README row, or the check catches it when
      someone forgets — `board close` moves the file, flips the status line and moves the row
- [x] the hand-written prose sections survive whatever the tool does to the tables

## Landed: the check. Still open: the generator

Closed once on the strength of the third box's `or` clause, then reopened — the title says
*generated*, and it isn't. What follows is what exists today; the remaining work is the
generate half described in Approach.

`crates/ansible-core/tests/board.rs`, run by plain `cargo test`. Not a script in `scripts/`:
the closest precedent is T-085's "a door only works if there is no window" guard in `fs.rs`,
and a test is already wired into the thing that runs on every change. It writes nothing, so
the prose sections cannot be clobbered — the fourth box is satisfied structurally rather than
by being careful.

It found **36** problems on the first run. The 27 this ticket predicted, exactly — 2 listed
open but sitting in `closed/` (T-018, T-033), 14 in `open/` with no row, 11 in `closed/` with
no row — plus **9 the audit missed**, because it only compared folders against the README and
never against the ticket's *own* status line. T-018 was in `closed/` still saying `open`;
T-033, T-066 and T-073 said `closed`, which isn't one of the three declared values. That is the
second of the three manual edits, and it drifts the same way the third does.

`partly done` is now an accepted status for a ticket in `open/` — T-029, T-031 and T-032 all
use it and the README prose already explains it. Flattening those to `open` would have thrown
away true information to satisfy a checker, so the checker learned the word instead.

Verified by simulating the failure it exists for: `git mv`ing an open ticket to `closed/` and
changing nothing else fails with all three edits named.

## Landed second: the lifecycle CLI (`crates/board`)

`cargo run -p board -- new|close|reopen|list|show`, std-only so it runs the same on Windows
and macOS. `close`/`reopen` do all three manual edits in one command and additionally strike
(`~~T-0NN~~`) / un-strike the closed ticket in other tickets' Blocked-by cells, so "is this
ticket workable" is readable off the board — `list --unblocked` computes it. Rows are edited
surgically (insert/move/remove one line), so hand-written table data — the P2 refs counts,
T-084's outcome note — survives untouched. Verified by a close/reopen round-trip against a
copy of this real board: the ticket file comes back byte-identical and the only README
residue is the reopened row's position in its table.

Not the generator the title asks for — that still needs the editorial-column question below
answered. The check stays the judge either way.

## What the generator still has to do

Rewrite the Open and Closed table blocks from the ticket headers, leaving every prose section
untouched. The check makes this safe to attempt, which was the whole argument for building it
first: a generator can now be judged by whether the check still passes and the prose still
reads, instead of by inspection.

Two things it needs that the check already worked out:

- **Where the data lives.** Every ticket's own header table carries status, priority, size and
  the depends/refs column; the README's copy is the duplicate. `board.rs` parses both sides
  already — `tickets()` and `rows()` are the two halves a generator would reuse.
- **What it must not touch.** Marker comments around each table block, so "Where the project
  actually is", the per-section notes and "Settled — don't re-derive these" are out of reach by
  construction rather than by regex care.

The open question is the columns the README carries that the headers don't, or carry only
sometimes — the P1/P2/P3 grouping, the reference counts in the P2 table (`385`, `2669`, `48`),
the `Blocked by` vs `Refs` split. Those are editorial and would have to move into the headers
first, or be preserved by the generator rather than regenerated. That inventory is part of this
ticket, as the Approach section already says.
