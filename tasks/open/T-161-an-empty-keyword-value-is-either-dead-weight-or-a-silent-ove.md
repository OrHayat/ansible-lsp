# T-161 — An empty keyword value is either dead weight or a silent override of an inherited one

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-128 | —          |

## Problem

A key written with no value — `vars_prompt:`, `ignore_errors:`, `become:` — is legal YAML and
loads to `None`. ansible-core accepts nearly all of them without a word. Measured on 2.21.2, of
twelve play keys given a null value, **eleven load clean**; only `tags:` errors
(`taggable.py:57`, `tags must be specified as a list`).

Silence is wrong in two different ways, and they need two different rules.

### Rule 1 — the null that does nothing

```yaml
- hosts: localhost
  vars_prompt:      # loads clean, contributes nothing
  tasks: []
```

`_load_vars_prompt` hands `None` to `preprocess_vars`, which returns `None`, and the loop that
would read it never runs (`play.py:238-247`). The key is indistinguishable from not writing it.
Same for `vars:`, `vars_files:`, `pre_tasks:`, `post_tasks:`, `handlers:`, `roles:`,
`environment:`, `module_defaults:`, `force_handlers:`, `serial:` — all measured clean.

Usually it is a half-finished edit: the author wrote the key, got distracted, and the file still
parses. Deleting the line is provably behaviour-preserving, so the quick fix is a plain delete.

### Rule 2 — the null that silently overrides, which is the one worth building

Inheritance is keyed on `Sentinel`, not on `None`. `_get_parent_attribute` consults the parent
only `if _parent and (value is Sentinel or extend)` (`task.py:540`), and `Sentinel` means *the
key was never written*. Writing the key with no value stores a real `None`, which is not
`Sentinel` — so the parent is never consulted and the inherited value is discarded.

Measured, and it changes what the play does:

```yaml
- hosts: localhost
  ignore_errors: true
  tasks:
    - name: fails
      command: /usr/bin/false
      ignore_errors:        # <- blocks the play's `true`
    - name: after
      debug: {msg: reached}
```

With that line: `fatal:`, the play stops, `after` never runs. Delete it: `...ignoring`, and
`after` runs. One empty key, and the play-level `ignore_errors: true` is gone — with no error,
no warning, and nothing on the line to suggest it did anything.

This applies to every inheritable `FieldAttribute` on a task, block or role — `become:`,
`remote_user:`, `check_mode:`, `any_errors_fatal:`, `connection:` and the rest. It does **not**
apply to `NonInheritableFieldAttribute` (`vars_prompt:` is one), which is why the two rules
split where they do.

**The fix here is to write `false`, not to delete.** Measured: `ignore_errors:` and
`ignore_errors: false` produce byte-identical PLAY RECAPs. The null *is* the false — `None` is
falsy, and `if not ignore_errors` (`strategy/__init__.py:560`) cannot tell them apart. So
writing it out changes nothing and makes the file say what it does, which is what a quick fix is
for. Deleting the line is the one edit that *does* change behaviour, by restoring the inherited
`true`. There is no choice to offer the author: only one of the two is behaviour-preserving.

### Rule 3 — the null that is simply invalid, and fails at run time

Not every attribute has a falsy twin. `connection:` is a plugin *name*, and `None` is not one:

```yaml
- hosts: localhost
  connection: local
  tasks:
    - command: /usr/bin/true      # ok
    - command: /usr/bin/true
      connection:                 # fatal: A non-empty plugin name is required.
```

Measured, and worse than rule 2 in two ways. The message names neither `connection:` nor a line,
so nothing in it points at the empty key. And it fails **with or without** a parent value — a
control run with the play-level `connection: local` deleted fails identically, and even an
explicit `-c local` on the command line does not rescue it, because the task's `None` beats the
CLI default too.

So this one is not an inheritance story at all: an empty `connection:` is a guaranteed run-time
failure, detectable at edit time, that ansible reports with a message giving the author nothing
to go on. Of the three rules this is the most valuable and the only one that earns ERROR.

### What the neighbours do

ansible-core: silent on all of it, except `tags:`.

ansible-lint: catches both, but only through its JSON schema, and only as a type complaint —

```
schema[playbook]: $[0].vars_prompt None is not of type 'object'
schema[playbook]: $[0].tasks[0].ignore_errors None is not of type 'boolean'
```

Note the schema is stricter than ansible itself, which accepts both. "None is not of type
'boolean'" reads as a typo to tidy; it does not say the play's `ignore_errors: true` was just
discarded. The detection is not novel — **the consequence is what nobody states**, and rule 2
exists to state it.

## Approach

One walk, three rule ids. What decides which is **what `None` means for that attribute**, not
where the key sits:

| when | rule id | severity | quick fix |
| ---- | ------- | -------- | --------- |
| `None` is invalid for the attribute (`connection:`) | `empty-value-invalid` | error | **none** |
| `isa='bool'`, and an ancestor sets it | `inherited-value-discarded` | warning | write `false` — the value it already has |
| `None` reaches the same result as absence | `empty-keyword` | hint | delete the line |

A quick fix is offered only where the edit is mechanical — where the null already *has* a
meaning and writing it out changes nothing. Row two is that case and only that case.

**Row one deliberately ships no fix.** An empty `connection:` is an incomplete edit, not a
mistake with a known repair: the author is part-way through typing a value, and the value is the
fix. We cannot know it, and the two edits we *could* make are both wrong — deleting the line
throws away what they were doing, and guessing a plugin name is worse. This is how every other
language server treats a half-written assignment: `x =` in Python and `x :=` in Go are reported
as errors and neither tool offers to delete the statement. Diagnose it, point at it, leave it
alone.

That is also why the severity is ERROR rather than a warning with a fix attached. It is not a
style smell to tidy — the file does not work, and the author already knows they are not finished.

Row three keeps its delete fix, but the same mid-edit argument applies in miniature: a hint
firing on `vars_prompt:` while the author is still typing the entries under it is noise. The
difference is that there the file works and the key genuinely does nothing, so the fix is a
legitimate cleanup rather than a guess. If it proves annoying in practice the answer is
debouncing, not dropping the fix.

Row three also absorbs the case where an inheritable key is empty but **no** ancestor sets it —
`None` and `Sentinel` then reach the same default, so it is dead weight again, not an override.
That check needs the enclosing play and block, which is a parent walk we already do, not the
reverse index — so this does not depend on T-020.

The three-way split has to be derived, not guessed: rule 3's `connection:` was found by testing
one non-bool, and the boundary between "invalid" and "has a falsy twin" is exactly `isa`. Walk
the attribute declarations rather than assuming bools are the only safe class.

Derive the inheritable/non-inheritable split from upstream rather than hand-listing it. The
class is right there in the source (`NonInheritableFieldAttribute` vs `FieldAttribute`), and a
hand-written list silently rots the next time upstream adds a keyword — the same argument that
put the keyword tables in `keywords.rs` under a generator.

Two things to settle by measurement before coding, because both can turn a hint into a false
positive:

- **`extend` attributes.** `task.py:540` reads `value is Sentinel or extend`, so an extending
  attribute (`tags:`, `vars:`) consults the parent *even when set*. A null there may merge
  rather than override. `tags:` already errors, so start with `vars:` and check whether the
  play's vars still reach a task that writes a bare `vars:`.
- **Roles and `include_role`.** A role's `defaults/` and a task's `vars:` are a different
  inheritance path again; confirm rule 2 fires only where the parent walk actually applies.

Quick fixes are new surface — nothing in `crates/` emits a `CodeAction` today. T-124 owns the
LSP protocol surface, so either it lands first or this ticket ships the diagnostics and leaves
the fixes to it. The diagnostics are worth having on their own; do not block rule 2 on the fix.

## Done when

- [ ] a null-valued key with no inherited value gets a hint on `empty-keyword`, and a quick fix
      deletes the line
- [ ] a null-valued **inheritable** key whose play or block sets it gets a warning on
      `inherited-value-discarded`, naming the value being discarded and where it came from
- [ ] that warning's quick fix writes the explicit equivalent (`ignore_errors: false`) rather
      than deleting the line, and a test asserts the two spellings still behave identically —
      the delete is the behaviour-changing edit, so it is not offered
- [ ] an empty `connection:` is an ERROR on `empty-value-invalid`, fired whether or not an
      ancestor sets it, since it fails either way
- [ ] that ERROR offers **no** quick fix, and a test asserts the code-action list is empty for
      it — an incomplete edit has no mechanical repair, and deleting the line would discard a
      value the author is still typing
- [ ] the three-way split is derived from each attribute's `isa`, so a keyword whose `None` is
      invalid cannot silently land in the hint bucket
- [ ] the `ignore_errors:` repro above is a test, pinned against both the inheriting and the
      non-inheriting spelling, so the rules cannot take each other's case
- [ ] the inheritable/non-inheritable split is derived from upstream's attribute classes, not a
      hand-written list, and the derivation is checked by the same drift test as `keywords.rs`
- [ ] `extend` attributes measured — a null `vars:` under a play with `vars:` either merges or
      overrides, and whichever it is, it is asserted
- [ ] `tags:` stays out of both rules — it is an upstream ERROR (`taggable.py:57`) and belongs
      to T-110 as a missing row, filed separately rather than absorbed here
- [ ] both rule ids `# noqa`-suppressible per T-010 and toggleable per T-025
- [ ] the quick fix for `inherited-value-discarded` offers the explicit value as well as the
      delete, and never applies one silently
