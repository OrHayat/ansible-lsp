# T-214 — Verdict values stringify integer literals so x == 0 and x == '0' are indistinguishable

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P1       | S    | T-121 | —          |

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

## Why this was filed P3, and why that was wrong

**As filed:** "Nothing the tool renders today is false. The hint reads `runs only if rc = 0`,
and with `rc` as the integer the source wrote, that is exactly right. A reader is not misled.
What is lost is the ability to *tell the two apart*."

**Measured on 2.21.2 while closing this, and it is false.** The type is not only lost for
consumers — it is lost for `matches_default`, which is computed as `(dflt == value) != negated`
on two values that have *already* been stringified. When the default's type differs from the
literal's, that comparison answers backwards, and `matches_default` is what picks "runs unless"
(runs by default) over "runs only if". With `rc` unset:

| condition | ansible-core 2.21.2 | the label we gave | |
| --- | --- | --- | --- |
| `rc \| d(0) == 0` | ok | runs unless rc changes from 0 | correct |
| `rc \| d(0) == '0'` | **skipping** | runs unless rc changes from 0 | **false** |
| `rc \| d(0) not in [0, 2]` | skipping | runs only if rc is not one of [0, 2] | correct |
| `rc \| d(0) not in ['0', '2']` | **ok** | runs only if rc is not one of [0, 2] | **false** |

Two of the four say a task runs by default when it does not, or the reverse. That is the tool
lying, which is this board's definition of P1 — so the priority is raised on the way out, and
the "nothing is false" reasoning above is kept only because the next person to read this
ticket should see why it was mis-sized rather than re-derive it.

The original reasoning was not careless: it is true for the rows where both sides share a type,
and those are the rows anyone writes down first. It only breaks where the *default* and the
*literal* disagree, and nothing in the stringified representation can show you that case exists.

## How it was found

The T-188 corpus verification turned each of the 65 single-verdict conditions gained by the
tree into a falsifiable prediction and ran all 128 against real ansible-core. 126 matched. The
two that did not are exactly the two rows above — the harness fed a *string* where the source
wrote an *integer*, and it could only do that because the verdict does not say which it was.

That is the whole finding: the mismatch was in the probe, and the probe could only be wrong
because the type is not carried. A verdict that recorded the literal's type would have told the
harness what to set.

## Decision — carry the type (2026-10-03)

Carried, as `Literal { text, kind }` beside `LitKind`, rather than rejected.

What settled it, beyond the false labels above:

- **The cost the ticket feared is not there.** `WhenEquals`/`WhenIn` have **no consumers
  outside `condition.rs`** — 35 mentions, all in that one file, ~10 of them non-test. "The cost
  is on every match arm" was right about the shape and wrong about the count.
- **`text` + `kind` instead of carrying `Const`.** `Const::Float(f64)` is not `Eq`, and
  `Verdict` derives `Eq`; keeping the rendered text and a fieldless kind tag preserves it, and
  makes the prose requirement hold by construction rather than by vigilance.
- **`excludes()` gains an answer it could not give.** `x == 0` and `x == '0'` are mutually
  exclusive and were reported as compatible.

`Literal::same_value` is Jinja's `==`, with every row measured as a `when:` on 2.21.2 rather
than reasoned from Python: numerics compare across `int`/`float`/`bool` (`0 == 0.0`,
`1 == true`, `0 == false`), a string equals only a string (`0 == '0'` and `true == 'true'` are
both false), and `none` equals only `none`.

## Approach

`Const` already distinguishes them — the parser produces `Const::Int` and `Const::Str` — so the
information exists and is discarded at the boundary into `Verdict`. Either carry `Const`
through, or add a small `Literal` enum beside the string.

Worth deciding deliberately rather than by default: the string form is what `label()` wants,
and every consumer today is a renderer. The cost of the change is on every match arm; the
benefit is entirely future. It may be right to close this as "won't fix, recorded" — in which
case say so here, because the next person to notice will otherwise re-derive it.

## Done when

- [x] a decision is recorded: carry the type, or reject this ticket with the reason
- [x] if carried: `x == 0` and `x == '0'` produce different verdicts, asserted, and the four
      corpus conditions above are pinned with the verdict each must produce
      — `an_int_literal_and_a_str_literal_are_different_verdicts`, and
      `the_four_corpus_conditions_frame_their_default_run_the_way_ansible_runs_them` pins the
      framing each one must carry against the measured run
- [x] if carried: `label()` still renders `0` and not `Int(0)` or `"0"` — the prose is right
      today and must not regress to make the type visible
      — `carrying_the_type_does_not_leak_into_the_prose`; the pre-existing label tests also
      pass unchanged, which is the stronger control
- [x] either way, the live table in Problem is re-run and still holds
      — re-run on 2.21.2, all four rows reproduce, both controls discriminate
