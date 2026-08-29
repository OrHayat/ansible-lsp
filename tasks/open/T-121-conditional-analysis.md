# T-121 — Conditional analysis

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | epic | P2       | L    | —          |

## Problem

`when:` is where Ansible hides its control flow, and it is the one place this project has
found a fault class nobody else checks: `when-import-var-mutated`, the `set_fact` inside an
imported playbook on the variable its own import is gated on. **ansible-lint has no rule for
it** — in 26.1.1 `import_playbook` appears only in `fqcn.py` — so that rule is novel rather
than a reimplementation.

Two shipped children (T-033, T-078) and five open ones, and the epic exists because the group
is half-finished in a way the individual files hide: **T-031 and T-032 are both marked "partly
done"**. The classifier, the four warning rules and the hints shipped; the tree consumer, the
code action and the computed mutual-exclusivity did not. Read separately each looks like a
ticket in progress; read together they are one surface that was taken to 60% and left.

The measured state: T-032 classifies **1053 of 2669** conditions in the corpus and finds
**zero** broken ones. That is a real result — the repo is clean on all four provable-fault
rules — and it also means the remaining 1616 unclassified conditions are where any future
finding lives.

Sequencing:

1. **T-117 first**, though it lives in T-114 rather than here — its cause is the 2.19
   templating rewrite, not conditionals as such. It audits the four rules that already ship
   against strict-conditional semantics. Rules that describe the wrong runtime should not be
   extended to more keywords.
2. **T-122**, which roughly quadruples the surface for free: `changed_when`, `failed_when` and
   `until` are the same expression language and get none of this today.
3. Then the unfinished halves of T-031 and T-032, and T-035's run profile.

## Children

- [ ] T-029 — Hover showing the candidates tried
- [ ] T-031 — `import_playbook` + `when:`: say what it actually does
- [ ] T-032 — Static `when:` evaluation
- [x] T-033 — Variables in `when:` that are defined nowhere
- [ ] T-035 — Evaluate `when:` under a supplied run profile
- [x] T-078 — `when:` explanation and module provenance fight over the module-name token
- [ ] T-122 — changed_when, failed_when and until are the same expression language
- [x] T-166 — when-import-var-mutated covers import_playbook only, and four more constructs flip the same way
- [x] T-213 — A defaulted membership test whose default is in the list is labelled runs only if
- [ ] T-214 — Verdict values stringify integer literals so x == 0 and x == '0' are indistinguishable
- [ ] T-215 — requirement() drops matches_default so an All clause that runs by default reads as required

## Done when

- [ ] every child is closed or rejected
- [ ] no child is left at "partly done" — either the remainder ships or it is split out and
      the ticket closes honestly
- [ ] the corpus classification rate is re-reported after T-122 widens the surface
