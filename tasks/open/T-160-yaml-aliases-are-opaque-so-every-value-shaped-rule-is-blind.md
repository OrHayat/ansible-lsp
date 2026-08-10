# T-160 — YAML aliases are opaque, so every value-shaped rule is blind through one

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P2       | M    | —          |

## Problem

The parser turns every YAML alias into a positioned `Node::Other` and keeps no anchor table
(`parse_libyaml.rs:215`). Ansible does the opposite: aliases are resolved by the YAML loader
*before* ansible sees the document, so every check runs against the resolved value.

That means any rule that inspects a value sees nothing through an alias. Measured on
ansible-core 2.21.2, all three fatal or clean exactly as if the value had been written inline:

```yaml
- hosts: localhost
  vars:
    bad: &bad {group: web}
    empty: &empty []
  tasks: []

- hosts: *bad     # Hosts list must be a sequence or string   (T-110 row 17)
  tasks: []
- hosts: *empty   # Hosts list cannot be empty                 (T-110 row 14)
  tasks: []
```

We are silent on both. A miss, never a false error — but a silent one, and it is not confined
to `hosts:`. Every rule that reads a value rather than a key is affected the same way:

| rule | what an alias hides |
| ---- | -------------------- |
| T-110 rows 14-17 | the whole `hosts:` value |
| T-110 row 20 | `with_items: *empty` — is the value missing or empty? |
| T-110 row 21 | `loop_control: *thing` — is it a mapping? |
| T-110 row 7 | `block: *empty` — an empty block still triggers the rescue rule |
| T-108 / T-109 | every `isa` coercion and every enum, when they land |
| T-102 | a duplicate key inside an aliased mapping |

The keyword rules are unaffected, since those read key *names*, which are always written out.

## Approach

### Substitute in the parser, not in the consumers

The alternative — keep a `Node::Alias` and have each rule resolve it — was considered and
rejected. Two reasons, and the first is the one that decides it:

**Ansible never sees an alias.** Its YAML loader resolves them before `Play`/`Task` are built,
so every check upstream runs on the resolved value. A tree that matches what that loader
produces means every rule is reasoning about the same data ansible did, for free. Keep aliases
in the tree and each rule has to model a thing ansible has no concept of — the same argument
that settled the `is_block` classifier: match upstream's model and the rules follow.

**The failure modes are asymmetric.** Substituting at parse, ~20 rules in `placement.rs` plus
`attributes.rs`, `condition.rs`, `references.rs` and everything T-108/T-109 adds get it with no
change and cannot opt out. Resolving at consumers, each one must *remember*, and forgetting is
a silent miss — the exact bug this ticket exists to fix. `Node::Other => {}` in
`placement.rs::hosts` was one instance of that class; consumer-side resolution would make it
the default.

What that costs, taken knowingly: the anchored subtree is **cloned** per alias site, and the
tree stops recording that a value came from an alias at all. The second is the one worth
mitigating — see the side table below.

### The work

libyaml already hands us everything; `build` discards it with `..`. `EventData::Alias` carries
`anchor: String`, and `Scalar`/`SequenceStart`/`MappingStart` each carry `anchor:
Option<String>`.

- Build `anchor name -> Node` while walking, and substitute on alias. Anchors are defined
  before use in a valid document, so one pass suffices.
- The substituted node keeps the **alias site's** span, not the anchor's. This is a deliberate
  divergence from upstream and the main thing to get right; it is what makes this more than a
  lookup. Measured on 2.21.2, ansible does the opposite:

  ```yaml
  vars:
    junk: &junk just a string
  vars_prompt:
    - *junk        # Invalid variable file contents. — Origin: repro.yml:2:11
  ```

  The origin is `2:11`, the anchor **definition**, not `4:7` where it was used. That is not a
  considered choice upstream: its loader tags each object with where it was parsed, and an alias
  yields the same tagged object, so the definition's position is the only one it has.

  We should point at the use site anyway, because a value can be perfectly legal where it is
  defined and illegal only where it is used — `empty: &empty []` is a fine variable, and
  `hosts: *empty` is the error. Blaming the definition would put the squiggle on a line with
  nothing wrong with it. Carry the definition as `DiagnosticRelatedInformation` instead, so both
  positions are reachable and the primary one is the line the author has to change.
- **Cycle guard.** `&a [*a]` is legal for libyaml to emit and would recurse forever. A seen-set
  or depth cap is the one genuine correctness risk in the change; without it a crafted file
  hangs the server.
- **Keep an alias side table on the document** — `alias site span -> anchor definition span` —
  rather than a `Node` field. Rules stay ignorant, which is the point, while go-to-definition
  on `*group1` and an unused-anchor hint stay possible later. A field on `Node` would push
  aliases back into every rule's match, which is what this design is avoiding.
- A merge key (`<<: *defaults`) is a *mapping* alias with different semantics again — it
  merges rather than replaces, and later keys win. Measure it before assuming; it may deserve
  its own row, since duplicate-key detection (T-102) has to agree with whatever it does.
- An alias to an undefined anchor is a YAML error the loader raises, so it never reaches us.
  Confirm that rather than assume it.

Worth checking against T-102 while in here: it runs a **separate** walk over the raw events
(`scan_dupes`, `parse_libyaml.rs:80`), not through `build`, so substitution will not reach it.
An anchored mapping with a duplicate key inside is therefore reported once at the definition
and not at each alias site. That may well be right — it is one authoring mistake, not N — but
it has to be decided and asserted rather than left to fall out.

## Done when

- [ ] `hosts: *bad` and `hosts: *empty` produce T-110 rows 17 and 14, anchored at the alias
- [ ] a recursive anchor (`&a [*a]`) terminates instead of hanging, pinned by a test — the one
      way this change can be worse than the silence it replaces
- [ ] the alias side table maps each alias site to its anchor definition, and no rule reads it
- [ ] a diagnostic through an alias points at the alias site, not the anchor definition, with
      the definition attached as related information — pinned by a span assertion, and recorded
      as a **measured divergence**: upstream points at the definition (`2:11` in the repro
      above), which is wrong for any rule whose fault depends on where the value is used
- [ ] merge keys (`<<:`) measured on 2.21.2 and either supported or documented as a miss
- [ ] an alias to an undefined anchor is confirmed to be a load error upstream, so it stays a
      parse failure rather than a silent `Other`
- [ ] T-102 agrees with the substitution: a duplicate key inside an anchored mapping is
      reported once per alias site, or once at the definition — settled and asserted either way
- [ ] `Node::Other` is documented as what remains after this: genuinely unexpected events only
