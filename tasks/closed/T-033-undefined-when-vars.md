# T-033 — Variables in `when:` that are defined nowhere

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| closed | —        | L    | —          |

**Split 2026-08-02.** The playbook-level unguarded class shipped as T-051's
`var-undefined`; the rest became per-feature tickets: **T-060** (`suspicious-var`
near-miss, the SILENT class), **T-061** (`undeclared-var` `-e` contract, the LOUD class),
**T-062** (ini inventories / extension-less group_vars — the index gap that inflated the
counts here). The corpus research below is preserved as the source data for those three.

## Problem

This is the fatal `when:` class, and it is invisible to every other tool because finding it
needs the **whole workspace**, not one file.

`condition::variables()` already extracts the root variables a condition depends on. Cross
those against every variable *defined* anywhere in the repo — `defaults/`, `vars/`,
`group_vars/`, `host_vars/`, `set_fact`, `register`, `vars_prompt`, `loop_var`, ini
inventories — and 70 of 1110 distinct roots are defined nowhere.

They split into two classes with very different consequences.

### LOUD — 9 variables, 55 uses: the play dies

Unguarded, so Ansible raises `'x' is undefined` and the run stops.

| Variable | Uses | Example |
| --- | --- | --- |
| `zfs_role` | 22 | `zfs_role == "storage"` |
| `s3_user_action` | 11 | `s3_user_action == 'create'` |
| `ad_join_node` | 6 | `inventory_hostname == ad_join_node` |
| `lustre_builder_mode` | 5 | `lustre_builder_mode is defined` |
| `infiniband_ip` | 4 | |
| `policies` | 4 | |

These aren't necessarily bugs — they're the playbook's **required `-e` inputs**. But nothing
documents that contract, so forgetting one fails mid-run against a live cluster.

### SILENT — 61 variables, 259 uses: the branch never runs, forever

Guarded by `default()` or `is defined`, so an undefined variable is swallowed. The condition
just quietly evaluates to its default and **nothing ever complains**. A typo here is
permanent and undetectable.

The near-misses are the payload — an undefined name one edit away from a name that *is*
defined:

| In `when:` | Actually defined | Uses |
| --- | --- | --- |
| `skip_build` | `_skip_build` | 8 |
| `snap_uuid` | `_snap_uuid` | 2 |
| `snap_ts` | `_snap_ts` | 2 |
| `podman_push_image` | `podman_**pull**_image` | 1 |
| `lustre_force_format` | `_lustre_force_reformat` | 1 |
| `use_pacemaker` | `ha_use_pacemaker` | 1 |
| `wait_ftpd_fail` | `wait_ftpd_fail_msg` | 1 |
| `postgres_save_image` | `postgres_acr_image` | 1 |
| `import_file` | `_import_file` | 1 |

`snap_uuid is defined` where the real variable is `_snap_uuid` is **always false**. That
branch has never run. The `_`-prefix mismatch appears repeatedly, which is what a convention
plus no checking produces.

**This is the diagnostic the project has been missing.** Every other rule so far catches a
broken *file reference*; this catches a broken *variable reference*, which fails more quietly.

## Approach

A variable index, built the same way as T-020's reverse index — the scan already walks every
file, so this is retaining data rather than new traversal.

```rust
pub struct VarIndex {
    defined: HashMap<String, Vec<Definition>>,  // name -> where it comes from
}
```

Sources to collect, in rough order of how easy they are to miss:

- `defaults/main.yml`, `vars/main.yml` keys (and any file under those dirs)
- `group_vars/`, `host_vars/` — both `x.yml` and `x/` directory form
- `vars:` blocks on plays, tasks, roles, `include_role`, and `roles:` dict entries
- `set_fact` keys, `register:` values, `vars_prompt` names, `loop_control.loop_var`
- **ini inventories and extension-less `group_vars` files** — grep for `name=`; these are
  not YAML and a YAML-only walk misses them, which inflated the first count
- `vars_files` targets (T-016), which means this benefits from that landing first

### Two rules

| Rule | Severity | Condition |
| --- | --- | --- |
| `undeclared-var` | INFORMATION | unguarded, defined nowhere -> must come from `-e` |
| `suspicious-var` | **WARNING** | guarded, defined nowhere, **and** within edit distance 1–2 of a name that is defined |

`suspicious-var` is the warning because the failure is silent and permanent. Plain
`guarded + undefined` with **no** near-miss is *not* reported — that's the legitimate
"optional feature flag" pattern and there are 259 uses of it. Only the near-miss earns a
warning; without that filter this rule is a 259-row false-positive machine.

Edit distance needs care. `use_pacemaker` vs `ha_use_pacemaker` is a prefix difference, not
a typo — it may well be a deliberate second flag. Scoring should weight `_`-prefix and
suffix differences separately from character substitutions, and the message must say
**"did you mean"**, never "this is wrong".

### Traps, all of which bit the exploratory version

Getting the extractor wrong makes this rule useless, and it was wrong three times:

- **string literals** — `transport_mode | default('rdma') == 'tcp'` looked like references to
  variables named `rdma` and `tcp`. Top of the results list, entirely bogus.
- **attribute access** — `lustre_mount_check.stat.exists` looked like `stat` and `exists`.
- **bare filter names** — `x | bool`, `y | length`, `z | int` looked like variables. `bool`
  came out as the single most-used "variable" in the repo at 821 uses.
- **Jinja tests** — `is defined`, `is not changed`.
- **magic variables** — `inventory_hostname`, `groups`, `hostvars`, `item`, `vars`,
  `ansible_*`.

`condition::variables()` handles all five and `ignores_literals_filters_tests_and_magic_vars`
pins each one. Any extension of this must extend that test first.

## Done when

- [ ] `VarIndex` collects every source above, ini inventories included
- [ ] the 9 LOUD variables are reported as INFORMATION, and no others
- [ ] the near-miss list is reported as WARNING, and plain guarded-undefined is silent
- [ ] `_`-prefix mismatches are distinguished from character typos in the ranking
- [ ] messages say "did you mean", never "wrong"
- [ ] `# noqa: undeclared-var` / `suspicious-var` both work
- [ ] corpus gate: total findings stay in the tens, not the hundreds — if the count explodes,
      the near-miss filter is wrong and the rule doesn't ship
- [ ] `scan` prints both lists, so the `-e` contract is documentable outside an editor
