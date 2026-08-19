# T-196 — missing-handler: warn where the handler set is provably closed

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P1       | M    | T-099 | T-028      |

## Problem

A `notify:` naming neither a handler nor a `listen:` topic is **fatal** — the play aborts with
`The requested handler '...' was not found in either the main handlers list nor in the
listening handlers list` — but only on a run where the notifying task reports `changed`.
Measured on 2.21.2, two files differing by one line:

| file                        | result                                          |
| --------------------------- | ----------------------------------------------- |
| nothing reports `changed`   | `exit=0`, `ok=2 changed=0 failed=0` — silent    |
| `changed_when: true` added  | `exit=1`, fatal                                  |

Same broken notify in both. So it survives every idempotent run, `--syntax-check` and
`--check`, and kills the first run that does real work. Upstream's own gate is recorded in
[`upstream/ansible-missing-handler.md`](../../upstream/ansible-missing-handler.md); this
ticket is the editor-side answer and does not depend on that being fixed.

Split from [[T-028]], which keeps the index and navigation. The reason for the split is risk,
not size: navigation cannot produce a false positive, this can, and the set where it is
provably safe turned out to be much narrower than T-028 assumed.

## Approach

Fire only where the handler set is **closed** — every contributor statically known, so absence
is provable rather than merely unobserved. All four conditions, measured:

1. **The play is in one file, with no dynamic `include_role` anywhere in it.** A dynamic
   include contributes handlers, and *order decides*: the same two roles, same play, only the
   include order swapped, goes from working to fatal. See [[T-197]], which diagnoses that case
   rather than suppressing it. Until then a dynamic include in the play means silence.
2. **No templated handler `name:` in scope.** A handler's `name:` **is** templated, so
   `name: "restart {{ svc }}"` matches `notify: restart nginx` — measured. One of those and no
   name in that scope can be proven absent. (`listen:` is the opposite: not templated, braces
   stay in the topic, so it matches nothing rendered — already reported by
   [`crate::static_fields`].) [[T-198]] owns the handler-side rule.
3. **The notify is literal.** A template cannot be matched by name. It must still reach the
   undefined-variable rules: an undefined var *inside* a `notify:` is fatal on **every** run,
   converged included, because `notify` is templated at `post_validate` before the module runs.
   So skip the *name* check here, never the variable check.
4. **Never from inside a role file.** A role does not know its plays, and cross-role notify is
   valid exactly when the other role is in the play — measured both ways. Needs [[T-020]].

The handler set itself is [[T-028]]'s index, and the matching rules live there: three spellings
per role handler, no comma splitting, nameless handlers skipped.

An orphan `listen:` topic is **not a second rule** — it is the same `handler is Sentinel`
branch and the same message, so one rule covers both.

Wording: `ERROR_ON_MISSING_HANDLER` (default `True`, `config/base.yml:1376`) downgrades the
failure to a warning with `exit=0`. Measured. The message should not promise a hard failure
without acknowledging the toggle.

## Done when

- [ ] a literal `notify:` matching no handler name and no `listen:` topic in a closed set warns
- [ ] an orphan `listen:` topic produces the *same* diagnostic, asserted to be one rule
- [ ] silent when the play contains a dynamic `include_role`, with a test naming that as the
      reason — a suppression nobody can see the reason for gets deleted later by accident
- [ ] silent when any handler in scope has a templated `name:`
- [ ] a templated `notify:` does not warn here, **and** the undefined-variable rule still fires
      on a variable inside it — asserted together, since the obvious implementation of the
      first breaks the second
- [ ] never fires from a role file, per condition 4
- [ ] `# noqa` suppressible, per T-010
- [ ] demo fixture with GOOD/BAD/NO HINT rows, pinned by an exact-diagnostic-set test plus an
      `every_other_demo_file_is_free_of_missing_handler` guard, per rule 4
- [ ] seen red before the fix, per rule 5
- [ ] corpus gate: measure first. Every hit is either a real latent fatal or a resolver bug,
      and which is which goes here with counts
