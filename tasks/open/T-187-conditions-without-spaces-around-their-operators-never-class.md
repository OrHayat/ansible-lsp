# T-187 — Conditions without spaces around their operators never classify

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P2       | S    | —          |

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

- [ ] `hosts|length>0` classifies identically to `hosts | length > 0`
- [ ] the same asserted for at least one arm that is **not** `RequiresNonEmpty` — the fix is in
      shared normalisation, so one arm passing proves nothing about the rest
- [ ] a quoted operator is not corrupted: `x == "a|b"` and `x == "a > b"` keep their literal
      values, asserted on the resulting `WhenEquals`
- [ ] the T-032 corpus classification percentage is re-measured and recorded here — it must not
      fall, and if it does not rise the fix did nothing worth having
- [ ] seen red before the fix
