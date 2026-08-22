# T-205 — A non-string vars_files entry is diagnosed as a missing file, which says the opposite of what happens

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | S    | T-099 | T-162      |

## Symptom

```yaml
vars_files:
  - 5
```

| | says |
| ---- | ---- |
| ansible-core 2.21.2 | `Invalid `vars_files` value of type 'int'.` — the play dies before its first task |
| us | `missing-file`, **WARNING**: "no file found for `5` — Ansible silently skips a missing `vars_files` entry, so its variables are never set" |

Both live-verified. The message is not merely incomplete, it is the negation of the truth: it
promises the play runs on without those variables, and the play does not start. A user who
believes it goes looking for a file named `5`, and the real fault is that the entry is an int.

P1 rather than P2 because we **speak falsely** here. That is the difference from this epic's
other children and from [[T-162]]'s own table, where the same root cause leaves a rule
*unwritten* and the tool merely quiet. [[T-087]] shipped the three shapes next to this one and
deliberately left it — this is that gap, written down.

## Cause

[[T-162]]: `Node::Scalar { value, span }` (`parse.rs:43`) keeps no scalar style, so `- 5` and
`- "5"` are one node. Measured through the analyzer — the two spellings produce byte-identical
diagnostics:

```
- 5      -> code=missing-file  sev=Warning  msg=no file found for `5` …
- "5"    -> code=missing-file  sev=Warning  msg=no file found for `5` …
```

And they need opposite answers. `- "5"` is a string: it passes ansible's type gate, is then a
genuinely missing file, and *is* silently skipped — the current message is **correct** for it.
So this cannot be fixed by pattern-matching the text. `value.parse::<i64>().is_ok()` would fire
on the quoted spelling, and a false error on correct code is the one thing this tool does not
do — the same wall [[T-162]] hit on `tags: [7]`.

Wider than ints. Any plain scalar YAML 1.1 resolves to a non-string is fatal the same way:
`- 3.14`, and the booleans `- yes` / `- no` / `- on` / `- off`. Only the int was measured; the
others share the gate and their message text is unverified, which is a thing to measure, not
to assume.

## Fix

Blocked on [[T-162]] delivering `implicit_type()`. Once it exists this is one arm:
a `vars_files` alternative whose implicit type is not `str` is
`invalid-vars-files-entry`, ERROR, naming the type ansible reports — the shape the other
three arms already have in `vars_files.rs`.

It must **not** also emit `missing-file`. That entry names no file, so the lookup is not a
fact about the mistake, exactly as the `import_playbook`-in-a-task-list filter already does
in `diagnostics_with`.

Considered and rejected as an interim, without [[T-162]]: softening the `missing-file` wording
so it stops claiming "silently skips". That trades a rare loud lie for a permanent loss of the
one thing that message exists to say, on the common case that is correct. Waiting is better.

## Done when

- [ ] `- 5` is `invalid-vars-files-entry` at ERROR, naming `type 'int'`
- [ ] `- "5"` keeps today's `missing-file` warning, unchanged and asserted in the same test —
      this is the control, and without it the fix is indistinguishable from a text match
- [ ] the fatal spelling emits exactly one diagnostic; `missing-file` does not also fire
- [ ] the float and the four YAML 1.1 booleans are **measured** against the installed core and
      then either covered or recorded here with the result
- [ ] a demo row in `demo/vars_files_demo.yml`'s fatal play, pinned like the other four, and
      the `every_other_demo_file_is_free_of_vars_files_entry_diagnostics` gate still passes
- [ ] seen red before the fix
