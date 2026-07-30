# T-036 — Parse the YAML that Ansible parses (lenient oracle, maybe a swap)

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | L    | —          |

## Problem

saphyr is a strict YAML **1.2** parser. Ansible is **PyYAML**, which prefers libyaml's
`CParser` when the C extension is present and falls back to pure-Python PyYAML otherwise —
both leniently accept input YAML 1.2 rejects. So there is a class of files that **run in
production but our parser refuses**, and for those we produce no references, no navigation,
and (since T-013) an `unparseable` hint.

The corpus has one today: `roles/lustre-nvme-binding/tasks/_run.yml:45`, a `vars:` value —

```yaml
    _summary: "{{
      (_nvme_binding_result.stdout_lines | default(['{}']))
      | reject('equalto', '')
      | list | last | from_json
    }}"
```

The closing `    }}"` is indented 4 spaces, the same as the `_summary:` key. Per YAML 1.2.2
(§7.3.1 double-quoted style, §6.8 flow folding, §8.2.2 block mappings) every continuation
line of a multi-line double-quoted scalar must be indented *more* than its key, so saphyr is
**spec-correct** to reject it. libyaml simply doesn't enforce that rule — a well-known
leniency shared across the PyYAML/libyaml lineage. Ansible runs the file fine.

This is not a saphyr bug and there is no fix to wait for:

- The check is hardcoded at `saphyr-parser/src/scanner.rs:2005-2010`
  (`if (self.mark.col as isize) < self.indent { Err("invalid indentation in quoted scalar") }`),
  present on both `v0.0.11` (latest tag) and `HEAD`/v0.0.12-dev.
- saphyr issues [#57](https://github.com/saphyr-rs/saphyr/issues/57) and
  [#73](https://github.com/saphyr-rs/saphyr/issues/73) are both **closed as invalid-yaml**.

So the fault the user actually hit — "your parser is bad" — is really "correctness isn't the
goal; matching Ansible is." Matching Ansible means matching **libyaml**.

## The lever

`libyaml-safer` (a pure-Rust port of libyaml, no C) is the only pure-Rust parser that is
**both** PyYAML-lenient **and** carries byte-offset source marks (`start_mark`/`end_mark`
with `index`, `line`, `column`). Verified: it parses `_run.yml`'s scalar and reports a span.
The other candidates each miss one requirement — `yaml-rust2`/`marked-yaml`/`serde-saphyr`
share saphyr's rejection; `serde_yaml`/`unsafe-libyaml` are lenient but archived and give no
node spans.

## Two ways to use it

**A — Oracle (smaller).** Keep saphyr for the AST and spans; add `libyaml-safer` only as a
yes/no "would Ansible accept this?" check. Emit `unparseable` **only when both parsers fail**.

- Kills the false positive: `_run.yml` stops being flagged.
- Makes T-013's severity question moot — a diagnostic that fires only on files broken for
  Ansible too can be a WARNING (or louder) without lying.
- Limitation: does **not** recover references *inside* those files — saphyr still can't build
  their AST, so navigation/diagnostics there stay absent, just no longer mislabelled.

**B — Swap (bigger, more complete).** Replace saphyr with `libyaml-safer` in `parse.rs` (the
only module allowed to touch the parser, kept that way precisely so this is a one-file swap).
Because it also carries marks, references *do* resolve inside the previously-rejected files.

- Risk: it's event-based (libyaml API), so we build the marked tree ourselves rather than
  getting saphyr's `MarkedYaml`. The flow-mapping key lookup that picked saphyr over
  yaml-rust2 in T-001 must be re-proven.
- Risk: marks land on the quote characters and use different offset conventions — re-verify
  the byte-span invariants the *Settled* table pins (character-vs-byte markers, the em-dash
  and emoji round-trip tests must still pass).
- Must re-run T-001's 734-file corpus and confirm ≥ saphyr's 733 parse, with spans intact.

## Recommendation

Ship **A** first — it's the contained change that removes the lie and unblocks a truthful
T-013 severity. Treat **B** as a follow-on if we want real analysis inside libyaml-only files,
gated on the corpus + span-invariant re-verification above. Don't do B blind: prove the marks
and flow-mapping lookup on the real corpus before ripping saphyr out.

## Done when

- [ ] `libyaml-safer` integrated as a validity oracle
- [ ] `unparseable` is emitted only when the lenient parse also fails
- [ ] `_run.yml` (and any other strict-only failure in `~/app/ansible`) is no longer flagged
- [ ] `scan` reports the count of "strict-1.2-only" rejections separately, so the size of the
      gap stays visible
- [ ] T-013's severity is revisited now that the hint only fires on genuinely-broken files
- [ ] (Option B, if taken) the em-dash / emoji span tests pass against `libyaml-safer` and the
      corpus parse count is ≥ saphyr's
