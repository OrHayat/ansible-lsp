# T-224 — Injected names are a prefix guess: a user's own ansible_ var loses its hover, a typo is never flagged, and a scoped magic name is always believed

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-112 | —          |

## Symptom

`is_injected` (`condition.rs:479`) is `MAGIC.contains(name) || name.starts_with("ansible_")`,
and every consumer asks it *before* looking for a definition. Three lies follow, all
measured:

| read                                                   | ansible 2.21.2 | hover                | undefined rule |
| ------------------------------------------------------ | -------------- | -------------------- | -------------- |
| `{{ ansible_custom }}`, set in the play's `vars:`      | fine           | **none**             | silent         |
| `{{ plain_custom }}`, set beside it (control)          | fine           | definition + value   | silent         |
| `{{ ansible_hostnme }}` — typo, nothing defines it     | fatal          | none                 | **silent**     |
| `{{ ansible_play_name }}` — real, always present       | fine           | **none**             | silent         |
| `{{ ansible_role_name }}` in a plain play task         | fatal          | none                 | **silent**     |
| `{{ ansible_loop }}` outside `loop_control: extended`  | fatal          | none                 | **silent**     |

Row 1 is the one a user hits first: their own variable has a definition one screen up and
the hover says nothing, because the prefix short-circuits `variable_hover_at`
(`main.rs:2321`) and `variable_defs_at` never sees the use (the comment at `main.rs:8048`
records this for T-178). Rows 3, 5, 6 are silence where a diagnostic is due. Row 4 is the
hover knowing two names (`ansible_playbook_python`, `ansible_version`) and nothing else.

What ansible provides, by scope, from a `varnames` run on 2.21.2 with `gather_facts: false`:

| scope                | `ansible_*` names present                                                                                                                                                                                                                                                                                                                                                              |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| every task           | `check_mode` `config_file` `connection` `current_hosts` `dependent_role_names` `diff_mode` `facts` `failed_hosts` `forks` `host` `inventory_sources` `module_compression` `pipelining` `play_batch` `play_hosts` `play_hosts_all` `play_name` `play_role_names` `playbook_python` `role_names` `run_tags` `search_path` `shell_executable` `skip_tags` `ssh_host` `ssh_pipelining` `ssh_timeout` `timeout` `verbosity` `version` |
| in a loop            | + `loop_var`; with `loop_control: extended`, + `loop` `index_var`                                                                                                                                                                                                                                                                                                                      |
| inside a role        | + `role_name` `collection_name`; `parent_role_names` `parent_role_paths` only when the role was included from another role (`role_include.py:182`, read not run)                                                                                                                                                                                                                       |
| delegated task       | + `delegated_vars`                                                                                                                                                                                                                                                                                                                                                                     |
| after `gather_facts` | + every `ansible_<fact>` — host-dependent, not enumerable statically                                                                                                                                                                                                                                                                                                                   |

The non-prefixed injected names have the same scope shape ([[T-222]]'s table: `item` in a
loop, `role_name`/`role_path`/`role_uuid` in a role, the rest everywhere), and `MAGIC`
flattens all of them to "always".

## Cause

Two things are conflated in one predicate: "no workspace file could define this" and
"ansible defines this". The first is false — `ansible_custom` is an ordinary variable with
an ordinary definition — and the second is only true for a finite table with scopes, plus
the open-ended fact set *after* facts are gathered. The prefix stands in for both, and is
consulted first, so a definition never gets a chance to answer.

`variable_uses` in `condition.rs:470` drops injected names at the scanner, so no rule can
even see them; `vars.rs:828` repeats the prefix test on top.

## Fix

One order, every consumer:

1. **Definition first.** If the workspace defines the name in scope, that is the answer:
   hover shows it, go-to-definition jumps to it, the undefined rule is satisfied. The
   prefix never pre-empts this.
2. **Else, the injected table.** A single table of names ansible sets, each with a scope
   (always / loop / extended loop / role / child role / delegated) and a one-line meaning.
   It replaces `MAGIC` and the prefix; [[T-222]]'s four names land in it.
3. **In scope here → hover.** If the resolver has a value (`ansible_playbook_python`,
   `ansible_version` today; `inventory_file`/`inventory_dir` via [[T-070]]; `playbook_dir`;
   `ansible_play_name`; `role_name`/`role_path` from the resolved role), show it. If not,
   show the generic line: what the name is and which layer sets it.
4. **Out of scope here → diagnostic.** `ansible_role_name` in a play task, `ansible_loop`
   without `extended`, `item` outside a loop: flagged, with the scope named.
5. **Not in the table → facts, or a typo.** After `gather_facts` (or `setup`/
   `gather_facts` tasks, or an unknown-gathering caller), an unknown `ansible_*` name may
   be a fact: silent, with the generic "may be a fact" hover. With facts provably off for
   the play, an unknown `ansible_*` is undefined like any other name.

Step 5 is the only place the prefix survives, and only under a facts-on condition. The
"facts on" verdict errs toward silence: a play whose gathering state is unknown (task file
with no known caller, `gather_facts` templated) keeps the blanket.

Rule 3: the table is the data, `is_injected` becomes `injected_scope(name) ->
Option<Scope>`, and each consumer — hover, go-to-definition, `variable_uses`,
`undefined_uses`, the injected hover — gets a test row from the first table above.

## Done when

- [ ] `{{ ansible_custom }}` with a play `vars:` definition hovers the definition and
      go-to-definition reaches it, with `plain_custom` as the control in the same test
- [ ] `{{ ansible_hostnme }}` in a `gather_facts: false` play is flagged undefined, and the
      same read in a `gather_facts: true` play is silent
- [ ] `{{ ansible_role_name }}` in a play task and `{{ ansible_loop }}` in a non-extended
      loop are flagged with the scope named; the same reads in scope are silent
- [ ] `{{ ansible_play_name }}` hovers a generic line; `{{ ansible_playbook_python }}` still
      hovers the install value (the existing test keeps passing)
- [ ] the table's comment names the 2.21.2 `varnames` recipe it was diffed against
- [ ] `MAGIC` and `starts_with("ansible_")` are gone from `condition.rs` and `vars.rs`;
      [[T-222]]'s test rows pass against the table
