# T-156 — with_<lookup> to loop: modernization, with autofix only where it is provably safe

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P3       | M    | T-128 | —          |

## Problem

`loop:` arrived in 2.5 as the simpler spelling and is what the docs recommend, but
`with_<lookup>` is **not deprecated** — upstream's wording is "that syntax will still be
valid for the foreseeable future". So this is a style rule, not a correctness one: HINT
severity, off by default, toggled by name (`modernize-loop`) through T-025's settings.

The trap is that the rewrite looks mechanical and is not. Two probes on 2.21.2
(`scratchpad/t110_modernize_probe.sh`):

```yaml
with_random_choice: "{{ my_list }}"   # ok: (item=b) — runs ONCE
loop: "{{ my_list | random }}"        # FATAL: The `loop` value must resolve to a 'list', not 'str'.
```

```yaml
with_items: "{{ [[a, b], [c]] }}"     # item=a, item=b, item=c   — flattens one level
loop: "{{ [[a, b], [c]] }}"           # item=['a','b'], item=['c'] — does not
```

The first turns working code into a hard failure. The second is worse: no error at all, just
a different number of iterations with differently-shaped `item`. A naive key swap ships both.

`with_<lookup>` is recognised only when the suffix names an **installed lookup plugin**
(`task.py:336`), so the rule's trigger set is machine-dependent and needs T-115's index —
the same dependency `placement.rs`'s deliberate over-reach on `with_frobnicate` is waiting on.

## Approach

Classify by what the rewrite actually does, and only autofix the classes where the target is
provably meaning-preserving.

| Class               | Lookups                                                                              | Target                                         | Autofix |
| ------------------- | ------------------------------------------------------------------------------------ | ---------------------------------------------- | ------- |
| direct              | `with_list`                                                                          | `loop: X`                                      | yes     |
| filter              | `with_items`, `with_nested`/`with_cartesian`, `with_together`, `with_subelements`, `with_sequence`, `with_dict` | `flatten(levels=1)`, `product`, `zip`, `subelements`, `range`, `dict2items` | yes |
| `item` changes shape| `with_indexed_items`                                                                 | `item.0`/`item.1` become `loop_control.index_var` | no   |
| lookup form         | `with_fileglob`, `with_first_found`, `with_lines`, `with_env`, `with_url`, `with_template` | `loop: "{{ query('fileglob', ...) }}"`     | no      |
| not a loop          | `with_random_choice`                                                                 | delete the loop, inline `| random` at the use   | no      |

The last three classes offer the target text in the message and leave the edit to the author.

Two things to settle before writing any of it:

- **T-129 triages this first.** ansible-lint may already own the rule; if it does, this
  becomes a *covered* or *port* entry rather than an invention of ours.
- **There is no autofix machinery.** The server registers zero `CodeAction` handlers today,
  so the first real cost is that plumbing, not the rule. Splitting it into its own ticket is
  reasonable if the classification lands first.

## Done when

- [ ] T-129 has triaged this against ansible-lint before any code is written
- [ ] every `with_<lookup>` in the table is classified, and the classification is a table in
      code, not scattered `match` arms
- [ ] the rule is HINT, and off unless `modernize-loop` is enabled (T-025)
- [ ] the trigger set comes from T-115's lookup index, not a hardcoded list — an uninstalled
      `with_x` is an invalid attribute upstream, not a loop (`task.py:336`)
- [ ] `with_items` autofixes to `flatten(levels=1)`, never a bare `loop:` — pinned by a test
      that would fail on the naive rewrite
- [ ] `with_random_choice` is suggest-only, and the message says the loop goes away
- [ ] no autofix is offered for a class marked "no" in the table
- [ ] `# noqa` suppression works, per T-010
