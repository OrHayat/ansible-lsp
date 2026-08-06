# T-130 — Ship it beyond the Extension Development Host

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | epic | P3       | L    | —          |

## Problem

This server currently runs in one place: an Extension Development Host launched with F5 from
this folder, on one machine, against one repo. Everything else on the board makes it better
at its job. This is the only group that makes it usable by anyone who is not the author.

Six children, and they are not all packaging — they are the things that have to be true
before a stranger's machine is a supported environment:

| Child | Blocks a stranger by |
| ----- | -------------------- |
| T-019 | there is no installable artefact |
| T-026 | it is VS Code only, and the colouring will not survive the move (see T-126) |
| T-025 | there is no way to turn a rule off you disagree with |
| T-027 | nothing proves it beats the plugin it replaces, on a repo neither of us chose |
| T-077 | the test suite reads `~/app/ansible` and **silently skips** where that is absent |
| T-081 | the board's tables are hand-edited and have drifted before |

**T-077 is the one to do first, and it is not optional.** Tests that skip silently on any
machine but one are not a safety net — they report success. Every other child here is
verified by a test suite that currently does not run away from home.

T-027 deserves its stated ordering: run the differential harness against the legacy
`ansible-role-goto` plugin over all 731 corpus files **before** T-019, because shipping a
`.vsix` that regresses against the thing it replaces is worse than not shipping.

Sized L and P3 honestly: none of this is urgent while the audience is one person, and all of
it is required the moment it is two. Grouping it means that decision gets made once,
deliberately, rather than discovered when someone asks for a download link.

## Children

- [ ] T-019 — Package as a .vsix
- [ ] T-025 — Settings: toggle rules, override severity
- [ ] T-026 — Neovim lspconfig entry
- [ ] T-027 — Differential harness vs the legacy plugin
- [ ] T-077 — Real-repo tests read a caller's home dir; move to inline fixtures
- [ ] T-081 — The board is hand-edited, and it has drifted
- [x] T-014 — README is stale
- [x] T-043 — Docs stale after the parser swap

## Done when

- [ ] every child is closed or rejected
- [ ] `cargo test` passes on a machine with no `~/app/ansible` and no Ansible installed, with
      no silent skips
- [ ] a second person can install it and use it without reading this repository
