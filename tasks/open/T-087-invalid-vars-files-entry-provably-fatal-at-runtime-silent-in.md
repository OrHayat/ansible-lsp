# T-087 — Invalid vars_files entry: provably fatal at runtime, silent in the editor

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | S    | —          |

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

## Done when

- [ ] null, mapping, and depth-2 items each get an ERROR diagnostic on their own span
- [ ] an entry resolving to a directory warns with "fails the play" wording, not
      "silently skips"
- [ ] `# noqa: invalid-vars-files-entry` suppresses it
- [ ] demo gains labeled cases and the LSP-level test pins them
