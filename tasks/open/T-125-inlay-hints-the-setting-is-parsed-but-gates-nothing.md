# T-125 — Inlay hints: the setting is parsed but gates nothing

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P2       | M    | T-124 | —          |

## Symptom

`ansibleLsp.inlayHints.enabled` appears in the VS Code settings, is read on
`didChangeConfiguration` into `Settings::hints`, and **does nothing**. Toggling it changes no
behaviour. There is no inlay-hint provider in `ServerCapabilities` and no handler —
`grep -rn "inlay_hint\|InlayHint" crates/ client/src/` returns nothing outside `node_modules`.

Three tickets still describe hints as shipped: [[T-032]]'s done-when refers to them, [[T-025]]
plans settings around them, and [[T-031]] says the `import_playbook` diagnostic "was dropped in
favour of an inlay hint (shipped)". `scripts/inlay-hints.py` is still in the tree, named for a
feature that is not there, and its own docstring calls the setting "the one switch that gates
the hover" — which [[T-078]] made false.

## Cause

A real provider existed: `inlay_hint_provider: Some(OneOf::Left(true))` plus an
`async fn inlay_hint` handler, added in `8ab9580` ("Analyse when: conditions and surface the
result as inlay hints") and removed in `a3da5b4` ("Replace when: inlay hint with a hover").

**Two things this ticket previously said about that removal were wrong, and both mattered:**

- It blamed [[T-078]]'s module-hover token conflict. `a3da5b4` states its own reason in the doc
  comment it added, and it is a UX call, not a collision: *"Hover, not an inlay: the explanation
  is wanted on demand, not painted onto every conditional line where it clutters the file and
  collides with the editor's own end-of-line blame. Hover has room to spell out every clause
  instead of a truncated stub."* [[T-078]] landed **later** and fixed the collision
  independently — `guard_line` appends to the reference hover instead of replacing it, and its
  Outcome records that "the `settings.hints` gate is gone from the hover path entirely". The two
  are unrelated.
- It said the setting "defaults to off", and concluded nobody has been misled yet because of
  that. It defaults **on** — `hints: true` in `impl Default for Settings`
  (`crates/ansible-lsp/src/main.rs:74`), pinned by
  `hints_default_on_and_only_an_explicit_false_disables_them`.

So the standing objection is only to *always-on painting of every conditional line*. It is not an
objection to inlay hints as a surface, and it does not reach a hint that fires a handful of times
per file.

## Fix

Not "remove or restore the `when:` hint". Decide **which derived facts earn an inlay**, then
implement those behind the setting that already exists.

Everything below is something the tool already computes and currently only surfaces on hover, or
does not surface at all. Ordered by how often it would fire, because fire-rate is the whole of
`a3da5b4`'s objection — the rare ones carry derived information without painting the buffer, and
are where this should start.

### Rare — a handful per file

| Site | Machinery that already exists | Hint |
| ---- | ----------------------------- | ---- |
| `include_tasks: "{{ env }}.yml"` | `path_substitution_hover` already computes the substituted value, its `VarSource`, and the resolved `→ target` | `→ prod.yml` |
| `include_tasks: x.yml` / `import_playbook` | `a3da5b4` deleted 104 lines of `mutation.rs` carrying per-import task/play counts keyed by resolved target | `→ 12 tasks` |
| `register: r` | [[T-192]] (check_mode fabricates values, not missing keys) and [[T-193]] (a looped register has only `results`) | `results only (looped)` |
| `notify: restart nginx` | `ast.rs` `HandlerRef`; [[T-157]] is the case where a block makes the name unnotifiable | `→ handlers/main.yml:12` |
| `delegate_to:` | [[T-105]] — empty template, host not in inventory | `→ web01` |
| A key on a `roles:` entry | [[T-100]] — an unknown key silently becomes a variable; `VarSource::RoleParams` vs `RoleEntryVars` already distinguishes it | `role param` |
| `ansible_version`, `ansible_playbook_python` | `injected_var_hover` already renders the value out of `AnsibleInstall::detected` | `2.17.6` |
| `when:` — the original | `Verdict::label()` (`condition.rs:173`) is an inlay renderer with **zero production callers** — every non-test call is `condition.rs` calling itself, and ~20 tests still pin it. Its `All` arm folds clauses into a count because "inline space is tight", which is an inlay constraint hover does not have | `runs only if mode = docker` |

### Frequent — most lines, so the clutter objection lands hardest here

| Site | Machinery that already exists | Hint |
| ---- | ----------------------------- | ---- |
| A variable use, `{{ pkg }}` | `vars.rs` — 17 `VarSource` variants with the precedence model, `vars::effective` to pick the winner, `source_label()` for the short name, and `def_value()` which is already whitespace-collapsed and capped at 60 chars "for a one-line hover" | `= nginx (role default)` |
| A short module name, `- debug:` | `module_hover` derives the winning collection — `ansible.builtin` / `ansible.legacy` / a collection name — and workspace-vs-installed | `ansible.builtin` |
| Any task | the same function's `is_action` / twin / platform search already decides where the code runs | `controller` |

The variable one is the strongest candidate on value: precedence across 17 sources is the
hardest thing about Ansible, `def_value` is an inlay renderer in everything but name, and the
answer currently costs one hover per variable. It is also the one that fires most, so it is the
one that has to be *measured* rather than argued.

Which of these ship is the decision this ticket makes. Whatever ships, the tree has to stop
saying hints are already here: fix [[T-031]], [[T-032]] and [[T-025]], and make
`scripts/inlay-hints.py` match — its filename and its docstring are both stale.

## Done when

- [ ] the setting works: a provider in `ServerCapabilities`, a handler, and toggling
      `ansibleLsp.inlayHints.enabled` visibly changes behaviour — asserted under both values,
      the way [[T-078]] asserted its hover under both
- [ ] the shipped set is named here in writing, and every candidate above is either shipped or
      has a recorded reason it was not
- [ ] each shipped hint has a test per site, not one test for the provider — the rule lives on
      the data, so every producer gets asserted (rule 3)
- [ ] no hint duplicates what the hover on the same token already says, or if it does, that is a
      deliberate recorded choice — `guard_line` and `Verdict::label()` are two renderings of one
      fact and must not both appear on one line
- [ ] the default is chosen deliberately and written down. It is `true` today and gates nothing;
      shipping frequent hints under a default-on setting is exactly what `a3da5b4` rejected
- [ ] judged against the real demo side by side, at a stated number of cursor positions, before
      it lands — [[T-078]] settled its anchoring question at twelve, and the clutter question is
      no more answerable by reasoning than that one was
- [ ] [[T-031]], [[T-032]] and [[T-025]] no longer describe inlay hints as shipped
- [ ] `scripts/inlay-hints.py` matches whatever was decided, docstring included
- [ ] `grep -rn inlay` across the repo returns nothing surprising
