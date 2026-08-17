# T-183 — scan drops an unreadable file silently, hiding its references from the gate

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | bug  | P1       | S    | —          |

## Symptom

`bin/scan` is the CI gate: non-zero exit when a literal reference names a file that isn't
there. A file the walk finds but **cannot read** was skipped without a word, so whatever was
inside it — including the broken references the gate exists to catch — went unchecked and
unreported, and the run exited clean.

Measured on the same tree, one permissions bit apart, so the probe reports either way:

| `hides.yml` mode | scan says                                          | exit |
| ---------------- | -------------------------------------------------- | ---- |
| `0644`           | `MISSING FILES (1): hides.yml:3  tasks/gone.yml`   | **1** |
| `0000`           | *(empty report)*                                    | **0** |

The readable row is the control that proves the gate was alive and the reference genuinely
broken; without it a green could just as well mean the fixture had nothing to find.

The headline made it worse rather than flagging it: it counts files from the **walk**, so the
unreadable file was still tallied — `1 files, 0 unparseable` over a file nobody opened. The
only trace was that the config header said `(2 files)` while the headline said `3 files`, a
discrepancy no human reads.

P1 by the board's own test — "the tool lies or goes silent". This is the silent half, in the
one command CI reads. Rare trigger (a root-owned file, a bad mount, a file in flight), but
nothing about the output invites suspicion when it fires.

## Cause

`bin/scan.rs`, the read at the top of the per-file loop:

```rust
let Some(src) = cache.source(path) else {
    continue;
};
```

`ScanCache::source` returns `Option` for two different situations — never looked at, and a
remembered read *failure* — and both collapse to `None` here. The `continue` was right; the
silence was not. Every finding list (`missing`, `broken_when`, `mutated`, `undefined_vars`) is
populated further down the same iteration, so a `continue` at the top means the file
contributes to none of them while still being counted by `files.len()` in the headline.

## Fix

Collect the skipped paths and print them. Three lines: a `unreadable: Vec<String>`, a push
before the `continue`, and an `UNREADABLE (n) — not analysed:` section, plus a third field in
the headline so `0 unparseable` can no longer stand in for "all fine".

**Not part of the exit code**, matching `UNPARSEABLE`, which is also reported without failing
the gate (`exit(if missing.is_empty() { 0 } else { 1 })`). A permissions bit is not an Ansible
fault, and a gate that goes red on one gets switched off — the same reasoning
`a_templated_path_does_not_fail_the_gate` already encodes, where failing on computed include
paths "would make the check unrunnable and it would be turned off". The repair is not "make it
red", it is "stop letting silence read as clean": the tool's job here is to say *I could not
look at this*, which converts a wrong answer into no answer.

Rejected alternative — fail the gate on unreadable — for the reason above. Worth revisiting
only together with `UNPARSEABLE`, since the two should not disagree.

## Done when

- [x] the two-run measurement above is a test, not a fixture that only proves the section
      prints — `an_unreadable_file_cannot_quietly_take_a_broken_reference_with_it` asserts the
      readable run goes red **and** names `gone.yml`, then that the unreadable run says so out
      loud. A scan that reverted to silent dropping fails the second half; one that stopped
      resolving fails the first.
- [x] the unreadable count is separate from `unparseable` in the headline and in its own
      section — they mean different things (`unparseable` is "Ansible would choke on this
      too", a permissions failure says nothing about the YAML), and
      `an_unreadable_file_is_named_rather_than_silently_dropped` asserts the full headline
      `3 files, 0 unparseable, 1 unreadable` rather than a substring that a merged count would
      still satisfy
- [x] both tests seen red before the fix, for the right reason
- [x] real trees unchanged: the 759-file corpus reports `0 unreadable`, `demo/` reports
      `110 files, 2 unparseable, 0 unreadable` — the new field is additive, and the two
      pre-existing unparseable files are still counted where they were
- [x] `bin/scan.rs` coverage holds: 97.33% regions, **100% functions**, 99.56% lines, with the
      new branch exercised rather than merely compiled
