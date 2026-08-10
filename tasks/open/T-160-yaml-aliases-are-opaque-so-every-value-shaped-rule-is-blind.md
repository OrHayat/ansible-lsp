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

Resolve aliases in the parser, where ansible's loader does it, rather than teaching each rule
about them. libyaml emits `YAML_ALIAS_EVENT` carrying the anchor name, and anchors arrive on
the scalar/sequence/mapping start events we already destructure — the fields are discarded by
the `..` today.

- Build `anchor name -> Node` while walking, and substitute on alias. Anchors are defined
  before use in a valid document, so one pass suffices.
- The substituted node keeps the **alias site's** span, not the anchor's, or every diagnostic
  jumps to the wrong line. That is the main thing to get right, and it is what makes this more
  than a lookup.
- A merge key (`<<: *defaults`) is a *mapping* alias with different semantics again — it
  merges rather than replaces, and later keys win. Measure it before assuming; it may deserve
  its own row, since duplicate-key detection (T-102) has to agree with whatever it does.
- An alias to an undefined anchor is a YAML error the loader raises, so it never reaches us.
  Confirm that rather than assume it.

Worth checking against T-102 while in here: an anchored mapping used twice means the same
duplicate key would be reported at both alias sites, or at neither, depending on where the
substitution happens.

## Done when

- [ ] `hosts: *bad` and `hosts: *empty` produce T-110 rows 17 and 14, anchored at the alias
- [ ] a diagnostic through an alias points at the alias site, not the anchor definition — the
      case that makes this worth doing properly, pinned by a span assertion
- [ ] merge keys (`<<:`) measured on 2.21.2 and either supported or documented as a miss
- [ ] an alias to an undefined anchor is confirmed to be a load error upstream, so it stays a
      parse failure rather than a silent `Other`
- [ ] T-102 agrees with the substitution: a duplicate key inside an anchored mapping is
      reported once per alias site, or once at the definition — settled and asserted either way
- [ ] `Node::Other` is documented as what remains after this: genuinely unexpected events only
