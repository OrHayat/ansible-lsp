# T-087 — Invalid vars_files entry: provably fatal at runtime, silent in the editor

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| done   | P2       | S    | T-099 | —          |

## Problem

Three `vars_files` entry shapes fail ansible-core's post-template type gate
(`vars/manager.py:348-353`, "A `vars_files` value should either be a string or list of
strings") and kill the play at start. All three live-verified on 2.21.2 during T-016:

```yaml
vars_files:
  -                 # null item          -> Invalid `vars_files` value of type 'NoneType'.
  - dir: vars/env   # mapping item       -> Invalid `vars_files` value of type 'dict'.
  - - - deep.yml    # depth-2 nesting    -> same gate, via the inner list
```

Plus the near-miss cousin, fatal at read time rather than at the gate: an entry naming a
**directory** ("[Errno 21] Is a directory") — even as a first-found alternative it fails
instead of falling through (Settled entry in the README).

The editor says nothing about any of these: `vars_files_of` (`ast.rs`) drops non-scalar
and empty items while building references — right for navigation (no path to reference),
wrong as the only pass, because code that provably cannot run stays unflagged. Ansible
itself errors loudly and immediately, so the gap is "the editor tells you before you run",
not "nothing ever tells you" — hence P2, not P1.

## Approach

A new rule (e.g. `invalid-vars-files-entry`) at **ERROR** severity — the unparseable-file
precedent applies: a play that loads this fails, so it is a real error, not a hint.
Detection belongs where the shapes are already seen and dropped: `vars_files_of` (or a
sibling walker) records the offending item's span and kind; the LSP maps them to
diagnostics alongside `missing-file`. The directory case rides the existing resolution
instead: a candidate that exists as a directory is distinguishable with one `is_dir`
probe, and fixes the message nit that "Ansible silently skips" is wrong for that sub-case.

## Outcome

Two rules, not one, because they make opposite claims about what happens at runtime and so
must not share a `# noqa`:

| rule | severity | fires on |
| ---- | -------- | -------- |
| `invalid-vars-files-entry` | ERROR | null / mapping / depth-2 items, at either level |
| `vars-files-directory`     | ERROR | an entry whose lookup stops at a directory |

Detection sits where validity was already being decided. `ast::vars_files_of` was dropping
these items with a `filter_map`; it now returns them as `Play::invalid_vars_files`, and
`vars_files::problems` reads the AST rather than re-walking the nodes. Its own doc comment
had called this ("a future ERROR-rule candidate") at exactly the right line. A second walker
would have been a second definition of what a valid entry is, free to drift from the one
navigation uses — rule 3.

**Re-measured on 2.21.2, and the ticket had one detail wrong:** depth-2 nesting reports
`type 'list'`, not the dict message. "Same gate, via the inner list" was right about the
gate and wrong about the text.

**Two shapes that look fatal and are not**, both now controls in the test suite: a null
`vars_files:` key, and an empty nested list (`- []`). Each runs. Flagging either would have
been a false positive on working Ansible.

**The directory case turned out to be a resolver bug, not just a message.** Measured: with
a directory at candidate 1 and a real *file* at candidate 2, ansible stops at the first
candidate that **exists** and dies — it does not read the file. We were reporting that entry
`Resolved` and offering go-to-definition into a file Ansible never opens. Same for a
directory alternative inside a first-match group: it fails rather than falling through to a
sibling that exists, so it no longer defers to the group as a merely-absent alternative
does. `Resolution::directory` carries the verdict; `rule_id_for` picks the id from it.

### Split out, not fixed: a non-string scalar

`- 5` is equally fatal (`type 'int'`) and is **not** covered here. `parse::Node::Scalar` keeps
no quoting style, so `- 5` and `- "5"` are one value to us — measured, they produce
byte-identical diagnostics — and they need opposite answers, so no text match can separate
them. Today such an entry gets `missing-file`, whose "silently skips" wording is the negation
of what happens. Filed as [[T-205]], blocked on [[T-162]].

## Done when

- [x] null, mapping, and depth-2 items each get an ERROR diagnostic on their own span —
      `every_fatal_shape_is_reported_with_the_type_ansible_names`, one row per measured
      type. A null item has no text, so its span widens onto the `-` rather than rendering
      as a zero-width squiggle (`a_null_item_is_anchored_on_its_dash`)
- [x] an entry resolving to a directory warns with "fails the play" wording, not
      "silently skips" — and the two messages are asserted against each other in the one
      file that carries both (`vars_files_demo_directory_entry_is_fatal_not_a_miss`)
- [x] `# noqa: invalid-vars-files-entry` suppresses it, and the other rule's id does not
      reach it — `noqa_suppresses_each_vars_files_rule_by_its_own_id`
- [x] demo gains labeled cases and the LSP-level test pins them —
      `vars_files_demo_flags_every_shape_that_cannot_name_a_file`, plus
      `every_other_demo_file_is_free_of_vars_files_entry_diagnostics` as the false-positive
      gate. Both new demo plays were run against real ansible in the demo's own layout and
      died as labelled (rule 4)
- [x] seen red before the fix, per rule: the AST arm broken (core tests red on `[]`), the
      resolver's `is_dir` broken (all three directory tests red), and `problems` stubbed to
      empty (the two LSP tests that depend on it red, the directory one correctly still
      green)
