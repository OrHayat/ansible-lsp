# T-187 — Conditions without spaces around their operators never classify

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | bug  | P2       | S    | —          |

## Symptom

The same condition classifies or not depending on whitespace. Measured:

| condition            | verdict                             | hint |
| -------------------- | ----------------------------------- | ---- |
| `hosts \| length > 0` | `RequiresNonEmpty { var: "hosts" }` | `runs only if hosts is non-empty` |
| `hosts\|length>0`     | **`Unknown`**                       | none |

Identical Jinja, identical meaning to Ansible, different answer from us. Unspaced filter chains
are ordinary in real playbooks, so this silently drops classifications — and the symptom is an
*absent* inlay hint, which nobody notices, rather than a wrong one.

Goes silent rather than lies, hence P2 rather than P1. It is the same defect class as T-186 —
both are the classifier being narrower than it claims — and the two should be fixed together
if convenient, but they fail independently and get their own tickets.

## Cause

`normalize` does not pad around `|`, `>`, `<`, `==` and friends, while every shape matcher is
written against the spaced spelling (`strip_suffix("> 0")`, `strip_suffix("| length")`). So the
matchers only ever see one of the two spellings Ansible accepts.

## Fix

Insert spaces around operators and pipes in `normalize`, then collapse runs of whitespace, so
every matcher sees one canonical form.

Do not do it by string replacement alone without checking what it breaks: `|` appears inside
quoted literals (`x == "a|b"`), `>` appears inside comparisons that are already spaced, and
`-` / `>` combine in Jinja's whitespace-control markers (`{%- ... -%}`) if a condition ever
carries one. Padding inside a quoted string would corrupt the value used by `WhenEquals` /
`WhenIn`.

The measurable target: T-032 records **39% of all conditions** and **93% of import-level** ones
as classified today, pinned by a corpus test. That percentage is the regression check — it must
rise, and the drift test is what proves the change did something.

## Done when

- [x] `hosts|length>0` classifies identically to `hosts | length > 0`
- [x] the same asserted for at least one arm that is **not** `RequiresNonEmpty` — the fix is in
      shared normalisation, so one arm passing proves nothing about the rest
- [x] a quoted operator is not corrupted: `x == "a|b"` and `x == "a > b"` keep their literal
      values, asserted on the resulting `WhenEquals`
- [x] the corpus classification rate is re-measured and recorded. Measured over the eight
      trees T-184 pinned (the `~/app/ansible` corpus the 39% came from is not reproducible —
      see [[T-188]]), running the same sweep against the pre-fix `classify`:
      **1541 → 1697 of 11 379 sites, 13% → 14%.** It rose.

      **Only 7 of those 156 are this ticket.** The bulk — 71 — is [[T-211]]'s `d(` alias,
      which the same rewrite fixed. Worth stating plainly rather than claiming the whole
      number: tight spacing is rare in real playbooks, and the seven are
      `configured_nameservers | length>0`, `docker_packages_list | length>0`, `item|length > 0`
      and four `…|length > 0` clauses inside multi-clause conditions.

      So this fix is worth having for correctness, not reach. The reach argument belongs to
      T-211 and the correctness argument is the nine wrong claims [[T-188]] removed.

- [x] seen red before the fix — the whole test ran against the pre-fix `classify` and printed
      `"hosts | length > 0" vs "hosts|length>0": RequiresNonEmpty vs Unknown`

**Root cause:** see [[T-188]] — the classifier matches strings; this is one symptom.
