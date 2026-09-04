# T-225 — A configured inventory path that does not exist drops the group_vars and host_vars beside it, which ansible still reads

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-112 | —          |

## Symptom

`ansible.cfg` says `inventory = inventory.yml`, the file is generated and not checked in, and
`group_vars/all.yml` sits next to where it would be. On a checkout without the generated file,
every variable defined only in that `group_vars/all.yml` is reported as `var-undefined`, with
no hover and no go-to-definition — the whole T-062 gap, back.

Measured on the reference tree (`volumez/matrix/ansible`, commit `186c7ed5`, 768 files),
`scan` on 2026-09-04:

| `inventory.yml` | `var-undefined` hits |
| --------------- | -------------------- |
| absent (the checkout as it is) | **434** |
| present, as an empty `all: {hosts: {}}` | **197** |

The 237 that vanish are, by grep, names defined in the root `group_vars/all.yml`
(`daos_build_output_dir:841`, `container_temp_dir:345`, `versity_submodule_path:587`,
`cib_batch_env:693`, `pulp_url:1388`, …). T-062's 238 was measured with the file present;
the same tree on a machine without it reads as a rule that regressed.

**Ansible reads that directory whether or not the file exists.** Live on ansible-core 2.21.2:

```
ansible.cfg:            inventory = elsewhere/inventory.yml     (no such file)
elsewhere/group_vars/all.yml:  x: FROM_ELSEWHERE_GROUP_VARS
group_vars/all.yml:            x: FROM_ROOT_GROUP_VARS
plays/p.yml:            - hosts: localhost … debug: msg="x={{ x | default('UNDEFINED') }}"

$ ansible-playbook plays/p.yml
[WARNING]: Unable to parse …/elsewhere/inventory.yml as an inventory source
    "msg": "x=FROM_ELSEWHERE_GROUP_VARS"
```

Three runs, so the answer could have come out otherwise: the file present gives the same
`FROM_ELSEWHERE_GROUP_VARS`; deleting `elsewhere/group_vars/` gives `UNDEFINED`, so the
root and playbook-adjacent copies were never the source, and the cwd was the root
throughout. The directory beside the *configured path* is what is read, and its existence
is what matters, not the inventory file's.

## Cause

`inventory::source_dirs` (`inventory.rs`) returns a directory only for a path that
`is_dir()` or `is_file()`; a missing path contributes nothing, and the comment above
`sources` calls that "the normal state where inventories are generated and untracked". That
is the right rule for `sources` — a missing file has no hosts — and the wrong one for
`source_dirs`, because the vars plugin keys off the path, not the file. `vars.rs:1593`
walks `source_dirs` for `group_vars`/`host_vars`, so the missing file takes its directory
with it.

## Fix

`source_dirs` yields the parent of a configured path that does not exist, when that parent
exists — the same answer as for a file that does. A directory source that is missing has no
basedir to give (measured? no: **measure it** before deciding; a missing `-i dir/` may or may
not read `dir/group_vars`, and the answer goes in the code comment).

Hosts stay as they are: `sources` still drops the missing file, and `unknown-host` still
answers from no hosts — that half is correct, and the status bar already says the file is
missing (T-202's `missing` list). This ticket is only about the vars beside it.

Re-measure after: the 434 must become 197 on the tree as checked out, by diffing the two
sorted lists — zero lines may appear that are not in the with-file run.

## Done when

- [ ] `source_dirs` yields the directory beside a missing configured file, pinned by a test
      with the control that a missing *directory* source behaves as measured on core
- [ ] the missing-directory case is measured on ansible-core and recorded in the code comment
- [ ] `var-undefined`, hover and go-to-definition all answer from `group_vars/all.yml` beside a
      missing `inventory.yml` — one test per consumer, each seen red without the fix
- [ ] the reference tree, as checked out, reports the with-file count (197 at `186c7ed5`)
      and the list diff shows no new line
- [ ] T-051's gate note and T-065 are updated with the corrected number
