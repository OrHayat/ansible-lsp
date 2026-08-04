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

- [ ] a script reports every disagreement between `tasks/{open,closed}/` and the README tables,
      and exits non-zero when there is one
- [ ] the 27 existing disagreements are fixed, so the check passes on a clean tree
- [ ] closing a ticket no longer requires hand-editing a README row, or the check catches it when
      someone forgets
- [ ] the hand-written prose sections survive whatever the tool does to the tables
