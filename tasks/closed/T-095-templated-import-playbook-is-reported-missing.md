# T-095 — Templated import_playbook says "missing file" when it means something else

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | M    | T-090 | —          |

## Symptom

`import_playbook: "{{ env }}-setup.yml"` gets the same yellow `missing-file` squiggle as
`import_playbook: setpu.yml`. Run with `-e env=prod` the first one resolves and runs; the
second never will. One message, two situations, and the wrong one for both.

**This ticket does not silence anything.** An earlier draft proposed routing it through the
never-warn templated path. That was wrong: a templated `import_playbook` is far *more*
constrained than an ordinary templated path, and most of the ways people write one are
provably broken. Going quiet would throw away the best diagnostic on this board.

## What Ansible actually does

`import_playbook` is expanded when the playbook file is **parsed** — before any host is
matched, before any play runs, before any fact exists. `PlaybookInclude.load`
(`playbook_include.py:69-83`):

```python
all_vars = self.vars.copy()
if variable_manager:
    all_vars |= variable_manager.get_vars()      # no play, no host, no task
templar = TemplateEngine(loader=loader, variables=all_vars)
new_obj.post_validate(templar)                   # templates import_playbook HERE
...
file_name = new_obj.import_playbook              # then the path is used
```

So it **is** templated — the current code's premise ("it can never reach a file") is false.
But `get_vars()` is called with no play, host or task, and its own docstring
(`vars/manager.py:168-188`) gates every source on exactly that context:

| Source | Reaches a parse-time `import_playbook`? |
| ------ | -------------------------------------- |
| extra vars (`-e`) | **yes** — the only ungated source |
| `vars:` written on the import entry itself | **yes** — `self.vars`, before the merge |
| magic vars (`playbook_dir`, …) | yes |
| `set_fact` (`fact_cache[host]`, `vars_cache[host]`) | **no** — needs a host |
| group_vars / host_vars | **no** — needs a host |
| play `vars:` / `vars_files` | **no** — needs a play |
| role defaults / role vars | **no** — needs a play |

That table is the whole ticket. Two of nine sources work. A `{{ env }}` fed by a `set_fact`
or a `group_vars/all.yml` — which is how most people would assume it works — fails **every
single run**.

### Live-verified, ansible-core 2.21.2

Not reasoned from source. Five cases, `ansible-playbook -i localhost, -c local`:

| `env` supplied by | Result |
| ----------------- | ------ |
| `-e env=prod` | **runs** |
| `vars:` on the import entry itself | **runs** |
| nothing | `'env' is undefined`, exit 4 |
| `group_vars/all.yml` | `'env' is undefined`, **exit 4** |
| `set_fact` in a preceding play | `'env' is undefined`, **exit 4** |

The failure is **fatal to the whole playbook**, not to the import:

```
[ERROR]: Error processing keyword 'import_playbook': 'env' is undefined
Origin: /tmp/t095/main_setfact.yml:6:20

4     - ansible.builtin.set_fact:
5         env: prod
6 - import_playbook: "{{ env }}-setup.yml"
                     ^ column 20
```

No `PLAY` banner is printed. The `set_fact` play **above** the import never runs either —
the file is parsed in full before execution starts, so one unresolvable import kills
everything in the file. That is what makes this an ERROR rather than a warning.

Note what Ansible does well here: file, line, column, caret, and the actual reason. It just
cannot say it until you run. **ansible-lint** is the one that misses it — it reports only
`Failed to find {{ env }}-setup.yml playbook.`, naming the wrong problem. Saying it before
the run, with the source of the variable named, is the gap.

## What ansible-lint does (checked, not assumed)

`utils.py:604-663`, `import_playbook_children`:

```python
possible_paths.append(lintable.path.parent / v)     # the raw string, braces and all
...
if not possible_path.exists():
    msg = f"Failed to find {v} playbook."
_logger.error(msg)
return []
```

It does not template it either — same naive join we do today. Two differences worth copying
the *shape* of and not the substance:

- it is a `_logger.error`, not a rule match: no rule id, no line, nothing to `# noqa`. It is
  ansible-lint admitting it could not follow the reference, not a claim the file is missing.
  Our squiggle makes a much stronger claim from the same information.
- it returns `[]`, so the imported playbook is silently **not linted**. Whatever rules would
  have fired inside it never run, and nothing says so.

So this is not a case of duplicating a rule a user already gets. Nobody gets this.

## Fix

Two verdicts where there is one today. **No claim about whether the run succeeds** — the
command line is invisible to us, so nothing here is provably fatal. An earlier draft of this
ticket proposed an ERROR tier on that basis and was wrong.

**1. WARNING, reworded — the file is not self-contained.** Keep warning, drop the false
"missing file" framing. The value is not a path on disk; it is a variable that must arrive at
launch, and requiring `-e` for a playbook's *structure* is the actual defect. Say that:
"`{{ env }}` is resolved when this file is parsed — only `-e env=…` or a `vars:` on this line
can supply it, so this playbook cannot be syntax-checked or linted standalone."

That last clause is the one **provable** statement in this ticket, and it is what justifies
warning at all. `--syntax-check` takes no user arguments, so it is a fixed experiment —
live-verified on 2.21.2:

| Command | exit |
| ------- | ---- |
| `ansible-playbook --syntax-check var.yml` | **4** — `'env' is undefined` |
| `ansible-playbook --syntax-check var.yml -e env=prod` | 0 |
| `ansible-playbook --syntax-check magic.yml` (`{{ playbook_dir }}`) | **0** |

So the file fails CI regardless of how anyone eventually runs it, and ansible-lint fails with
it — `has_playbook` shells out to exactly this check. **Magic variables are exempt**: row
three passes clean because `_get_magic_variables` needs no play or host. The rule is
"templated with a non-magic variable", not "templated". `expand_magic` (`resolve.rs:107`)
already knows which names those are.

Shares its shape with T-061's `-e` contract; whoever lands second reuses the first one's
lookup.

**The escape hatch is `# noqa`, and the message must say so.** "I do pass `-e env=prod`" is a
legitimate answer to this warning, and the author is the only one who can give it. The
mechanism already exists and already works here — `rule_id` returns `templated-import`
(`resolve.rs:73`) and `diagnostics_of` filters on `is_suppressed(r.span.start, rule_id(r))`
(`main.rs:472`), so `# noqa: templated-import` silences it today. What is missing is that
**nothing tells the user**. A warning that cannot be turned off by the person who knows the
answer is a warning that gets the whole tool disabled, so the message ends with the line to
add:

```yaml
- import_playbook: "{{ env }}-setup.yml"  # noqa: templated-import
```

Needs a test pinning the suppression, since it is now load-bearing rather than incidental.

**2. RESOLVED / navigable — expandable.** A literal `vars:` on the import entry, or a known
literal from T-056, expands the path. Offer the candidates like any other reference. Reading
that `vars:` is new plumbing: `references.rs` currently keeps only the import's value, not its
sibling keys.

A **literal** (non-templated) `import_playbook` that misses keeps today's `missing-file`
warning unchanged — that part was never wrong.

**Split out to T-136:** naming *which* definition the author is relying on ("`env` is defined
in `group_vars/all.yml`, which cannot reach here"). That needs the var index and its own
corpus check; this ticket is the message and the resolution behaviour. T-136 depends on the
`vars:` reading landing here first, or it false-positives on the one spelling that works.

## Size

Not S, despite the small diff at `resolve.rs:285-297`. The work is: routing `ImportPlaybook`
into the glob path (it is currently the `_ =>` arm at `resolve.rs:308`, so it has no search
bases), new reference plumbing to read a sibling `vars:`, flipping a test that pins the bug,
rewriting a Settled entry, and updating the demo. Five files, one of them the trust anchor.

## `import_tasks` is NOT the same — leave it alone

An earlier draft said "same three tiers, do it in the same pass". Live run, 2.21.2:

| `import_tasks: "{{ env }}-tasks.yml"` with | result |
| ------------------------------------------ | ------ |
| play `vars:` | **runs** |
| `-e env=prod` | **runs** |
| `set_fact` in an earlier task | exit 4, `Error when evaluating variable in import path` |
| nothing | exit 4 |

`import_tasks` is expanded inside a play, so **play vars and `vars_files` reach it** — exactly
what its own error text says ("vars/vars_files or extra-vars … not facts or inventory"). Its
legal source set is far wider than `import_playbook`'s, and it globs happily today
(`task_search_dirs`) without warning. Making it warn would false-positive on every templated
`import_tasks` fed by ordinary play vars — of which the corpus has plenty.

So: **no change to `import_tasks` in this ticket.** The only shared fact is that `set_fact`
reaches neither, which is T-136's territory if anyone wants it there.

## Watch out

- `resolve.rs:285-297` is the current arm, and its comment states the false premise in
  writing. Rewrite the comment, don't just change the branch.
- `templated_import_playbook_is_reported_not_skipped` **pins the bug**. It has to flip, and
  the tests replacing it should cover all three tiers.
- `tasks/README.md`'s Settled section asserts *"A templated `import_playbook` can never
  resolve … the only place `{{ }}` means 'wrong' rather than 'unknown'"*. That is false and
  has to be rewritten — not deleted. The wrong reasoning is worth keeping next to the
  correction, the way T-093's Cause keeps its struck-through claim.
- `demo/imported_semantics.yml` and the demo README describe the old behaviour.
- The live run is **done** (above) and the source table matched it exactly. Ansible runs
  under WSL on this machine (`ansible-playbook`, core 2.21.2); the controller cannot run on
  native Windows — `import ansible` dies on `grp`. The fixture is `/tmp/t095` in WSL, rebuilt
  by `scratchpad/t095.sh`.

## Done when

- [x] a live run of all cases is recorded here, and the table above matches it
- [x] a templated `import_playbook` no longer says "missing file"; it says what can supply it
- [x] the message names the provable harm — it cannot be syntax-checked or linted standalone
- [x] a magic-variable template (`{{ playbook_dir }}`) does **not** warn; it passes clean
- [x] it never claims the run will fail — `-e` is invisible to us
- [x] one expandable from a literal `vars:` on the entry resolves and offers candidates
- [x] a literal missing `import_playbook` still warns exactly as it does today
- [x] the message names `# noqa: templated-import` as the answer for "I do pass `-e`",
      and a test pins that the suppression works
- [x] ~~`import_tasks` gets the same three tiers~~ — live run says its sources are wider
      (play vars work); leave it unchanged, see above
- [x] the Settled entry is rewritten with the correction and the old reasoning kept

## What shipped

`ast.rs` — `Import` gains `vars: Vec<(String, String)>`, the literal scalars of a `vars:` on
the import entry. `references.rs` — `Reference::entry_vars` carries them to the resolver.
`resolve.rs` — the early "templated import is always Missing" arm is gone; entry vars are
substituted first, then `expand_magic`, and only a value still holding a **non-magic**
variable warns. Two bugs fell out of the reordering that the old arm had been hiding:

- the `ImportPlaybook` match arm ignored `substituted`, so an expanded `{{ playbook_dir }}`
  was computed and thrown away and the braces were joined onto `file_dir` instead. The
  magic-variable form could never have resolved even with the early arm removed.
- a `vars:` on the entry was never read anywhere in the codebase.

`main.rs` — `message_for` rewritten: names both working sources, the `--syntax-check` harm,
and `# noqa: templated-import`.

Tests: `templated_import_playbook_resolves_when_parse_time_can_supply_it` (both working
forms plus the warning case, one tree), `demo_import_playbook_forms_resolve_as_documented`
(the demo is asserted, not trusted), `templated_import_message_names_the_fix_and_noqa_silences_it`
(message content + the suppression it advertises). The existing
`templated_import_playbook_is_reported_not_skipped` still passes — its assertion was always
right, only its stated reasoning was false, so its doc comment was rewritten rather than the
test flipped.

`demo/playbook.yml` gained the three new forms next to the old one. Corpus impact: none — the
board's scan reports 73/73 `import_playbook` resolved and 0 missing, so it contains no
templated ones.
