# T-034 — Templating that looks dynamic but isn't

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P2       | M    | T-120 | T-015      |

## Problem

`{{ }}` is treated as "runtime unknown, glob and never warn". That was wrong for
`role_path` — 4 real references were fully knowable and sat unnavigable until they were
expanded. The same mistake is repeated in other shapes.

Measured across all 731 files, counting only templated values in reference position:

| Pattern | Count | Knowable |
| --- | --- | --- |
| `{{ item }}` over a **literal** `loop:` | 12 | **yes, exactly** |
| `{{ item }}` over a variable `loop:` | 13 | no |
| `lookup('env', 'VAR')` | 5 | yes, controller-side |
| `\| default('literal')` | 4 | yes, when unset |

Plus the ones already handled: `role_path` (4), `playbook_dir` (4), `inventory_dir` (0).

**11 of the 12 literal-loop cases are `template:`/`copy:` `src:`**, which the extractor
does not yet cover. Hence the dependency on T-015 — building this first would resolve
almost nothing.

## The one that matters

```yaml
template:
  src: "{{ item }}.j2"
loop: ['alertmanager.container', 'vmalert.container']
```

The value is not runtime data. It is a literal list two lines below the reference. Today
this globs `*.j2`; it should resolve to exactly two files, and warn if either is absent.

`TaskContext` already reads `loop:`/`with_*` to set `repeated`, but discards the values.
Capturing them is the same change that `conditions` needed.

## What `| default('literal')` actually costs today (measured 2026-08-17)

`substitute_literals` accepts only a **bare identifier** — any filter and the whole value is
abandoned. So the commonest defensive idiom in Ansible never substitutes, and the reference
falls through to globbing, which does not merely miss the answer: it returns wrong ones.

Two candidate files, one variable with a known literal:

| value                                       | result |
| ------------------------------------------- | ------ |
| `{{ shared_dir }}/inc.yml`                  | 1 target — `tasks/inc.yml`, substituted |
| `{{ shared_dir \| default('tasks') }}/inc.yml` | 2 targets — `other/inc.yml` **and** `tasks/inc.yml`, globbed |

The decoy is what makes this readable. With only the real file present both rows say
"Resolved" and the filter looks supported — a probe that cannot fail. The second file is the
control that separates substitution from a glob that happened to land.

Worth noting for the implementation: `{{ x | default('lit') }}` is **always** knowable, which
is stronger than the table above suggests. If `x` has a known literal, that value wins; if `x`
has no definition anywhere, the literal wins. Two candidates at worst, never zero — strictly
better than the glob.

Real instance, and it is knowable twice over:
`ad_keytab_path: "{{ samba_ctdb_deploy_dir | default('/opt/samba-docker') }}/krb5.keytab"`
in `roles/ad/defaults/main.yml`, where `samba_ctdb_deploy_dir` is also defined outright at
`group_vars/all.yml:452` — and `GroupVarsAll` is already an accepted literal source, so the
value is sitting in the `literals` map when the filter throws it away.

That one is a *hover* win, not a navigation win: the path names a file on the managed host,
not in the repo, so it must never be resolved as a workspace reference — only explained.

## Approach

Extend the expansion that `role_path` uses, rather than adding a parallel mechanism.
`expand_magic` already returns *several* candidate strings and a flag for whether any
`{{ }}` survived — that shape covers all of these:

| Source | Expands to |
| --- | --- |
| `item` + literal `loop:` | one candidate per list entry |
| `\| default('x')` | the default value |
| `ternary('a', 'b')` | both branches |
| `lookup('env', 'HOME')` | the controller's value |
| `first_found` with a literal list | each entry, first-hit-wins (already `from_candidates`) |

Rules that keep it honest:

- **Every entry must be literal.** One `{{ }}` inside the loop list and the whole thing
  stays unknown. 13 of the 25 `item` cases are exactly this.
- **A partially-expanded value is still templated** — glob it, never diagnose.
- **`vars:`/`set_fact` literals are deliberately excluded.** They look knowable but sit
  under 22 precedence levels; inventory or `-e` can override them, so expanding one would
  invent a false certainty. This is the line between "the value is in the file" and "the
  value is probably this".
- `lookup('env', …)` reads the *controller's* environment, which is right for
  `template`/`copy` `src:` and wrong for anything that runs on the managed host — so it
  needs T-015's local-vs-remote table, not just the string.

## Also here: the name a templated `set_fact` key creates (from T-169)

```yaml
vars:
  result_name: my_result
tasks:
  - set_fact:
      "{{ result_name }}": true      # creates a fact called `my_result`
```

`set_fact` and `set_stats`' `data:` are the only two places Ansible renders a mapping key
(upstream: "a rare case where key templating is allowed"). T-169 made the `result_name`
*use* navigable and stopped the index filing a definition called `{{ result_name }}`, but
the fact this really defines — `my_result` — is indexed under no name at all. Working it
out is expanding a template to a literal, which is this ticket.

Note it cuts against the exclusion above, and deliberately: that rule refuses to trust a
`vars:`/`set_fact` *value* as a path, because inventory or `-e` can override it. Here the
expansion produces a **name**, and a wrong name mis-files a definition rather than
inventing a missing file — so it wants the same expander with a different verdict on
partial knowledge, not the same answer. Decide that before implementing, not after.

## Done when

- [ ] a literal `loop:` expands `{{ item }}` to one candidate per entry
- [ ] a variable `loop:` leaves it templated, and a test pins that
- [ ] `| default('literal')` and `ternary(a, b)` expand
- [ ] `vars:`/`set_fact` are **not** consulted, and a comment says why
- [ ] corpus gate: zero new warnings across 731 files
- [ ] `scan`'s templated-variable survey re-run; anything still unresolved is genuinely
      runtime, and this ticket records the list
- [ ] a templated `set_fact`/`set_stats` key whose name is knowable indexes the fact under
      the **rendered** name, and one that isn't stays the deliberate miss T-169 left
