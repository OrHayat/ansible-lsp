# T-131 — Performance and responsiveness

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | epic | P2       | M    | —          |

## Problem

Everything else on this board is about being *right*. This is the only group about being
**fast enough that being right is worth anything** — a correct diagnostic that arrives after
the user has moved on is a diagnostic they did not get.

Three shipped, two open, and they are the same story told at two layers:

| Child | Cost | Outcome |
| ----- | ---- | ------- |
| T-074 | — | instrumentation, which is what made the other four arguable |
| T-075 | the startup scan blocked every request | fixed |
| T-084 | cold `ansible --version` blocked startup | fixed, **and two thirds of the ticket was rejected on the measurement** |
| T-076 | the var index re-walked shared files once per consumer | part 1 landed: 346 edges collapsed to 61 files on the demo |
| T-085 | the remaining 61 walks are syscall-bound — `statx` 1332/459, `readlink` 712/147, ~4.3× repeats | open |

The reason to group them is **T-084's lesson**, which is the most useful thing on this board
and is currently buried in a closed ticket nobody will open again: the instrumentation showed
detect costs 1 ms and the subprocess is never reached on a working install, so options B and C
were killed. *The measurement cancelled most of its own ticket.*

That is the standing rule for every child here: **measure first, and be willing to close the
ticket instead of doing it.** T-085 in particular claims a ~860× difference between `/mnt/c`
and ext4 — a number that decides whether it is urgent or irrelevant depending on where the
user's repo lives, and one that should be re-measured before any work starts.

T-055 (cache the cross-file variable index) is the fourth performance ticket and lives under
T-112 instead, because its subject is the variable index rather than the server's
responsiveness. Cross-referenced rather than moved — a ticket has one parent, and that one is
better placed where it is.

## Children

- [x] T-074 — Startup scan metrics
- [x] T-075 — The startup scan blocks all requests
- [x] T-076 — Var-index re-walks shared files once per consumer
- [x] T-084 — Cold `ansible --version` blocks startup for seconds
- [ ] T-085 — The var walk is syscall-bound, and 4× of the syscalls are repeats

## Done when

- [ ] every child is closed or rejected
- [ ] T-085 is re-measured on both a native filesystem and `/mnt/c` before any work starts,
      and closed unmeasured-as-unnecessary if the numbers do not hold
- [ ] startup time and scan time are reported by the existing metrics, so a regression here
      is visible without re-deriving the instrumentation
