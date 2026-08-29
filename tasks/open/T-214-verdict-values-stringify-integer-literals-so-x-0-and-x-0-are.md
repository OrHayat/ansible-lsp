# T-214 — Verdict values stringify integer literals so x == 0 and x == '0' are indistinguishable

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P3       | S    | T-121 | —          |

## Problem

`Verdict::WhenEquals.value` is a `String` and `Verdict::WhenIn.values` is a `Vec<String>`, so
a literal's type is lost on the way in. `x == 0` and `x == '0'` are different conditions in
Jinja and produce byte-identical verdicts.

Measured on ansible-core 2.21.2 — the two spellings behave *oppositely*:

| condition | `rc` | result |
| --- | --- | --- |
| `rc not in [0, 2]` | `0` (int) | skipping |
| `rc not in [0, 2]` | `"0"` (str) | **ok (ran)** |
| `rc \| d(0) == 0` | `0` (int) | **ok (ran)** |
| `rc \| d(0) == 0` | `"0"` (str) | skipping |

Both source conditions are real, from `debops`:

- `cryptsetup__register_ciphertext_blkid.rc not in [0, 2]` → `values: ["0", "2"]`
- `keyring__register_gpg_key.rc | d(0) == 0` → `value: "0"`

## Why this is a task and not a bug

Nothing the tool renders today is false. The hint reads "runs only if `rc` = 0", and with `rc`
as the integer the source wrote, that is exactly right — the third row above confirms it. A
reader is not misled.

What is lost is the ability to *tell the two apart*. That matters for anything that consumes a
verdict as data rather than as prose, and it matters for a user whose `rc` came from a `shell`
task's `stdout` — a string — where the hint is right about the condition and useless about
their run. Neither is a lie today, so P3.

## How it was found

The T-188 corpus verification turned each of the 65 single-verdict conditions gained by the
tree into a falsifiable prediction and ran all 128 against real ansible-core. 126 matched. The
two that did not are exactly the two rows above — the harness fed a *string* where the source
wrote an *integer*, and it could only do that because the verdict does not say which it was.

That is the whole finding: the mismatch was in the probe, and the probe could only be wrong
because the type is not carried. A verdict that recorded the literal's type would have told the
harness what to set.

## Approach

`Const` already distinguishes them — the parser produces `Const::Int` and `Const::Str` — so the
information exists and is discarded at the boundary into `Verdict`. Either carry `Const`
through, or add a small `Literal` enum beside the string.

Worth deciding deliberately rather than by default: the string form is what `label()` wants,
and every consumer today is a renderer. The cost of the change is on every match arm; the
benefit is entirely future. It may be right to close this as "won't fix, recorded" — in which
case say so here, because the next person to notice will otherwise re-derive it.

## Done when

- [ ] a decision is recorded: carry the type, or reject this ticket with the reason
- [ ] if carried: `x == 0` and `x == '0'` produce different verdicts, asserted, and the four
      corpus conditions above are pinned with the verdict each must produce
- [ ] if carried: `label()` still renders `0` and not `Int(0)` or `"0"` — the prose is right
      today and must not regress to make the type visible
- [ ] either way, the live table in Problem is re-run and still holds
