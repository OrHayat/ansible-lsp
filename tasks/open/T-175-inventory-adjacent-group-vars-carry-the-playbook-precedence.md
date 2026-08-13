# T-175 — Inventory-adjacent group_vars carry the playbook precedence level

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P2       | M    | —          |

## Symptom

A name defined in both `group_vars/all.yml` (beside the playbook) and
`inv/group_vars/all.yml` (beside the inventory) resolves to the wrong one when the two
disagree. Hover and go-to-definition both answer from `effective()`, so both point at the
file no run reads.

Measured on 2.21.2, one variable in both files:

```
inventory-gv vs playbook-gv -> src=FROM_PLAYBOOK_GV
```

The playbook copy wins. We rank them equal, so the winner is decided by the tie-break
instead — currently the later path, which is arbitrary between these two.

## Cause

`VarSource` has one `GroupVars` and one `GroupVarsAll`, and `precedence()` assigns them **7**
and **5** — Ansible's *playbook* levels. Its doc comment says so outright:

> Only the sources we index; the inventory-adjacent copies (next to a separate inventory
> file) and extra-vars (22) aren't here.

That was true when it was written. T-062 box 4 then started indexing the inventory-adjacent
pair (`read_var_dir(&dir.join("group_vars"), …)` off `inventory::source_dirs`) and reused the
same two variants, so the comment became stale and the level became wrong in the same commit.
Nothing failed, because no test had two copies of one name.

Ansible publishes six distinct levels here, not two:

| level | source                              | we say  |
| ----- | ----------------------------------- | ------- |
| 3     | inventory file/script group vars    | 6       |
| 4     | inventory `group_vars/all`          | 5       |
| 5     | playbook `group_vars/all`           | 5       |
| 6     | inventory `group_vars/*`            | 7       |
| 7     | playbook `group_vars/*`             | 7       |
| 8     | inventory file/script host vars     | 6       |
| 9     | inventory `host_vars/*`             | 10      |
| 10    | playbook `host_vars/*`              | 10      |

`VarSource::Inventory` at **6** is the other half of this and is separately suspect: an INI
`[web:vars]` section is "inventory file or script group vars" (3) and a host-line `k=v` is
"inventory file or script host vars" (8). The existing comment justifies 6 as "the lower of
the two" citing 6 and 10, which are not those two levels. Measure each before changing it —
the comment may be describing the right idea against the wrong numbers.

## Fix

Split the variants so the level is carried by the data rather than inferred at the read site:
`GroupVarsAll` / `InventoryGroupVarsAll`, `GroupVars` / `InventoryGroupVars`, `HostVars` /
`InventoryHostVars`. `read_var_dir` already knows which pair it is walking — the call sites
beside the playbook and beside `source_dirs()` are distinct — so the split costs a parameter,
not a new resolution step.

Every level in the table above needs its own probe before it lands. This is the ticket that
must not be written from the docs page: CLAUDE.md rule 1 exists because the published
precedence list is exactly the kind of source that reads as settled and is not.

Consumers to assert individually (rule 3): `effective()` — used by go-to-definition and by
the hover/inlay value — and `source_label` in the LSP, which names the source in the hover and
would start claiming "inventory group_vars" where it said "group_vars".

## Done when

- [ ] each level in the table above is measured with a two-file probe that could report either
- [ ] inventory-adjacent `group_vars`/`host_vars` carry their own `VarSource` variants
- [ ] `VarSource::Inventory`'s level is re-measured for both the `[g:vars]` and host-line cases
- [ ] `effective()` picks the playbook copy over the inventory copy, pinned by test
- [ ] hover's source label distinguishes the two, pinned by test
- [ ] the stale "aren't here" comment on `precedence()` is gone
- [ ] corpus count recorded; a move in either direction is explained before it lands
