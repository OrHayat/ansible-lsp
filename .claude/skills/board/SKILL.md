---
name: board
description: Work with this repo's ticket board (tasks/). Use when asked to find, pick, create, close, reopen, or update a ticket or epic, file a bug, list the backlog, check what's blocked/unblocked, or anything mentioning T-0NN ids, tasks/open, tasks/closed, epics, the upstream/ dossiers, or the board.
---

# The ticket board

One markdown file per ticket in `tasks/open/` and `tasks/closed/` — **status is the folder**.
`tasks/README.md` holds the human-facing tables plus hand-written prose. Never edit the
README tables or move ticket files by hand; the `board` CLI does every lifecycle edit and
keeps the README in sync. `cargo test -p ansible-core --test board` is the drift check.

## Commands

```
cargo run -q -p board -- list [-p P1|P2|P3] [-s S|M|L] [-k task|bug|epic] [-e T-0NN] [--unblocked] [--closed]
cargo run -q -p board -- show T-0NN
cargo run -q -p board -- new "Title" -p P1|P2|P3 -s S|M|L [-k bug|epic] [-e T-0NN] [-b T-020,T-062] [--problem "text"]
cargo run -q -p board -- close T-0NN [--rejected]
cargo run -q -p board -- reopen T-0NN
cargo run -q -p board -- sync T-0NN
cargo run -q -p board -- upstream [NAME]
```

Global flags: `--dry-run` (print planned changes, write nothing), `--dir <path>` (tests only).
Exit codes: 0 ok, 1 bad usage, 2 operation failed (message on stderr).

## Kinds

`-k` picks the body template, so it must be right at `new` time — changing it later means
rewriting the sections by hand.

| Kind   | For                                          | Sections scaffolded        |
| ------ | -------------------------------------------- | -------------------------- |
| `task` | default — work to do                         | Problem · Approach         |
| `bug`  | we shipped it wrong; the tool lies today      | Symptom · Cause · Fix      |
| `epic` | a parent that only exists to hold children    | Problem · Children         |

Kind is a column in the ticket's header table, read **by name** — tickets written before
kinds existed have no such column and count as `task`. Non-task rows carry a `**bug** ·`
badge in the README; task rows are unbadged.

There is no `upstream` kind, and `new -k upstream` is rejected on purpose — see below.

## Epics

`new "Child" -e T-090` writes both halves of the link: an `Epic` column in the child, and a
`- [ ] T-0NN — Title` line in the epic's `## Children` list. `close` ticks that box,
`reopen` unticks it. `show` on an epic prints children with state read from the **folders**,
not the checkboxes, so a stale box can't lie.

- The epic link is **not** a blocker. A child is workable the moment it's filed and still
  appears in `list --unblocked`. Use `-b` for real gating; `-e` only groups.
- `close` on an epic with open children is **refused** and names them. `--rejected` drops
  the epic anyway (for an epic that turned out to be the wrong framing).
- `-e` must name an existing ticket whose kind is `epic`; both are checked before anything
  is written, so a typo can't leave a half-filed child.

## Upstream dossiers

A bug in `ansible/ansible` lives as prose in `upstream/*.md` and is **never also a ticket** —
one finding, one home. `board upstream` indexes them (issue count, and whether they're filed
upstream, derived from the github links in the text); `board upstream <name>` lists one
dossier's issues. Nothing about that format is CLI-owned — the files are written for humans
and the parse reads them as-is, so don't reformat one to suit the tool.

What *we* do about an upstream bug is an ordinary ticket that cites the dossier path.

## Recipes

- **"Find me an easy ticket"** → `list -s S --unblocked`, then `show` the candidates and read
  the ticket files before recommending. P1 = the tool lies or goes silent, P2 =
  coverage/usability, P3 = nice-to-have. S <½ day, M ~1 day, L multi-day.
- **Create** → pick the kind first (`-k`), since it decides the sections. `new` scaffolds the
  file and README row; then WRITE the ticket body by editing the created file — the
  scaffold's sections are empty and a ticket with an empty body is not done being created.
- **Filing a batch** → create the epic first, then each child with `-e <epic id>`; the epic's
  `## Children` list builds itself in creation order. Don't hand-write that list.
- **"Is this a bug or a task?"** → `bug` means the tool's current behaviour is wrong and a
  user would see it (a false diagnostic, a wrong jump target, a lie in a hover). Missing
  coverage that never claimed to work is a `task`. When it's genuinely both, file the bug —
  P1 is defined as "the tool lies or goes silent", and that's the same test.
- **Close** → `close T-0NN` (or `--rejected` for decided-against). This also strikes
  `~~T-0NN~~` through the id in other tickets' Blocked-by cells — struck means no longer
  blocking. Close only when every `- [ ]` box in the ticket's "Done when" section is checked;
  the boxes are the definition of done, not the README or TODO.md.
- **Edit a ticket** → body text: just edit the file. Status: `close`/`reopen`, never the
  status line by hand. Header fields (priority/size/depends): edit the file's header table,
  then run `sync T-0NN` — the drift check fails until you do.

## Conventions that bite

- Ids are `T-` + 3 digits and are never reused, even by rejected tickets.
- `partly done` is a valid status for a ticket in `open/` — don't "fix" it to `open`.
- Header tables vary in width now (`Kind` and `Epic` are conditional). Read cells by header
  name, never by position — a positional read takes `Kind` for the priority.
- An epic is a container, not work. Don't give it "Done when" boxes that duplicate its
  children, and don't size it as if someone will sit down and do it.
- The README's prose sections ("Where the project actually is", "Settled — don't re-derive
  these", per-table notes) and hand-typed table data (P2 refs counts, outcome notes) are
  editorial. The CLI leaves them alone; you should too.
- After any board operation, `cargo test -p ansible-core --test board` must pass — run it if
  you did anything unusual.
