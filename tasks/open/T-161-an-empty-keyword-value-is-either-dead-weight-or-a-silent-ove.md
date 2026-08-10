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

**The fix here is not a delete.** Deleting the line restores inheritance, which is a behaviour
change — possibly the one the author wanted, possibly not. If they meant to switch the setting
off for this task, the correct edit is `ignore_errors: false`. We cannot know which, so the
diagnostic offers both and picks neither.

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

One walk, two rule ids, split by what the key is upstream:

| upstream | rule id | severity | quick fix |
| -------- | ------- | -------- | --------- |
| `NonInheritableFieldAttribute`, or normalized to a default at load | `empty-keyword` | hint | delete the line |
| inheritable `FieldAttribute` **and** an ancestor sets it | `inherited-value-discarded` | warning | delete, or write the explicit value |
| inheritable `FieldAttribute`, no ancestor sets it | `empty-keyword` | hint | delete the line |

The third row matters: with no ancestor setting the key, `None` and `Sentinel` reach the same
default, so it is rule 1 again. That check needs the *enclosing* play and block, which is a
parent walk we already do — not the reverse index, so this does not depend on T-020.

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
- [ ] the `ignore_errors:` repro above is a test, pinned against both the inheriting and the
      non-inheriting spelling, so the two rules cannot take each other's case
- [ ] the inheritable/non-inheritable split is derived from upstream's attribute classes, not a
      hand-written list, and the derivation is checked by the same drift test as `keywords.rs`
- [ ] `extend` attributes measured — a null `vars:` under a play with `vars:` either merges or
      overrides, and whichever it is, it is asserted
- [ ] `tags:` stays out of both rules — it is an upstream ERROR (`taggable.py:57`) and belongs
      to T-110 as a missing row, filed separately rather than absorbed here
- [ ] both rule ids `# noqa`-suppressible per T-010 and toggleable per T-025
- [ ] the quick fix for `inherited-value-discarded` offers the explicit value as well as the
      delete, and never applies one silently
