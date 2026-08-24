# T-209 — Model meta/main.yml whole, not just its top-level key names

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P3       | M    | T-106 | —          |

## Problem

T-147 routed `meta/main.yml` to `KeyContext::RoleMetadata` and checks its top-level key
*names*. Nothing reads the values, and nothing knows a key's extent:
`attributes::role_metadata_problems` walks `entries` and touches only `k.as_str()`.

That is enough to say "this key is not legal here" and not enough for anything else:

- **an autofix cannot delete a key.** Removing `galaxy_info:` means removing its nested
  block too, and the span we carry is the key token alone. [[T-210]] is blocked on exactly
  this.
- **the value types are unchecked.** `allow_duplicates: "yes please"` is `isa='bool'`
  (`metadata.py:37`) and would raise at load; we are silent. That is T-108's coercion rules
  applied to this file rather than a new set.
- **the three keys with their own schemas each need the parse first** — `dependencies:` is a
  list of RoleInclude ([[T-164]]), `argument_specs:` has the arg-spec schema ([[T-149]]),
  and `galaxy_info:` is Galaxy's, not core's.

## Approach

A typed read of the file, sitting beside the key check rather than replacing it:

- every top-level entry keeps the span of **key and value together**, so a fix can delete the
  pair and a diagnostic can underline the whole thing
- each of the four own keys gets its declared `isa` checked (`bool`, `list`, `dict`, `dict`),
  reusing T-108 rather than hand-rolling
- the 22 inherited `Base` keys are read as values but asserted about only as far as their
  `isa` goes — what they *mean* here is [[T-210]]'s question, and the answer measured so far
  is "nothing"

`galaxy_info:` sub-keys are **out of scope**: that schema belongs to ansible-galaxy, which
does no validation at all (`galaxy/role.py:125` is a bare `yaml_load`), so there is no
upstream oracle to mirror and inventing one would break rule 1.

## Done when

- [ ] each top-level entry carries a span covering key *and* value, asserted on a nested
      `galaxy_info:` block — the case a key-only span gets wrong
- [ ] the four own keys' `isa` is checked, with `allow_duplicates: "yes please"` flagged and
      `allow_duplicates: yes` silent
- [ ] a value-level fault and a key-level fault on the same file both report, neither
      swallowing the other
- [ ] `galaxy_info:` contents stay unread, with a test naming that as deliberate
- [ ] the corpus gate from T-147 (`role_metadata_corpus`) re-run and still 0 on the four
      named trees — a value check is where false positives on published roles would appear
