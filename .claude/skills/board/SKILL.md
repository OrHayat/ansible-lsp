---
name: board
description: Work with this repo's ticket board (tasks/). Use when asked to find, pick, create, close, reopen, or update a ticket, list the backlog, check what's blocked/unblocked, or anything mentioning T-0NN ids, tasks/open, tasks/closed, or the board.
---

# The ticket board

One markdown file per ticket in `tasks/open/` and `tasks/closed/` — **status is the folder**.
`tasks/README.md` holds the human-facing tables plus hand-written prose. Never edit the
README tables or move ticket files by hand; the `board` CLI does every lifecycle edit and
keeps the README in sync. `cargo test -p ansible-core --test board` is the drift check.

## Commands

```
cargo run -q -p board -- list [-p P1|P2|P3] [-s S|M|L] [--unblocked] [--closed]
cargo run -q -p board -- show T-0NN
cargo run -q -p board -- new "Title" -p P1|P2|P3 -s S|M|L [-b T-020,T-062] [--problem "text"]
cargo run -q -p board -- close T-0NN [--rejected]
cargo run -q -p board -- reopen T-0NN
cargo run -q -p board -- sync T-0NN
```

Global flags: `--dry-run` (print planned changes, write nothing), `--dir <path>` (tests only).
Exit codes: 0 ok, 1 bad usage, 2 operation failed (message on stderr).

## Recipes

- **"Find me an easy ticket"** → `list -s S --unblocked`, then `show` the candidates and read
  the ticket files before recommending. P1 = the tool lies or goes silent, P2 =
  coverage/usability, P3 = nice-to-have. S <½ day, M ~1 day, L multi-day.
- **Create** → `new` scaffolds the file and README row; then WRITE the ticket body (Problem /
  Approach / Done when) by editing the created file — the scaffold's sections are empty and a
  ticket with an empty body is not done being created.
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
- The README's prose sections ("Where the project actually is", "Settled — don't re-derive
  these", per-table notes) and hand-typed table data (P2 refs counts, outcome notes) are
  editorial. The CLI leaves them alone; you should too.
- After any board operation, `cargo test -p ansible-core --test board` must pass — run it if
  you did anything unusual.
