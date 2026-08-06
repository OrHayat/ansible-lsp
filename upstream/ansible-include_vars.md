# Upstream issues to file against ansible/ansible — `include_vars`

Local drafts, not filed yet. All live-verified on ansible-core 2.21.2 (Homebrew ansible
14.2.0); the action plugin is byte-identical between 2.21.2 and 2.22.0.dev0, and issue 1 was
also confirmed present in 2.20.4. Line numbers below are from the 2.21.2 tree.

---

## Issue 1 — `ignore_files` is documented as file names but treated as end-anchored regexes

**Component:** `lib/ansible/plugins/action/include_vars.py`,
`lib/ansible/modules/include_vars.py` (docs)

**Summary.** The docs for `ignore_files` say *"List of file names to ignore."*
(`modules/include_vars.py:54`). The implementation treats each entry as a regular
expression, end-anchored (`plugins/action/include_vars.py:207`):

```python
if re.search(r'{0}$'.format(file_type), filename):
```

Two consequences a user reading the docs cannot predict:

- An entry matches any filename that *ends* with it, not just the named file.
- Regex metacharacters are live — the `.` in `bastion.yaml` matches any character.

**Reproduction.**

```
v/bastion.yaml        from_bastion: 1
v/edge-bastion.yaml   from_edge: 2
v/keep.yaml           from_keep: 3
```

```yaml
- hosts: localhost
  gather_facts: false
  tasks:
    - include_vars:
        dir: v
        ignore_files: [bastion.yaml]
    - debug:
        msg: "bastion={{ from_bastion | default('SKIPPED') }} edge={{ from_edge | default('SKIPPED') }} keep={{ from_keep | default('SKIPPED') }}"
```

**Actual output (2.21.2):**

```
"msg": "bastion=SKIPPED edge=SKIPPED keep=3"
```

`edge-bastion.yaml` is silently skipped even though only `bastion.yaml` was listed.

**Expected.** Per the docs, only `bastion.yaml` should be skipped.

**Suggested fix.** A docs fix, not a behavior change (existing playbooks may rely on the
regex semantics): document `ignore_files` the way `files_matching` five lines above already
is — *"regular expression"* — and state the end-anchoring. Alternatively `re.escape` +
exact match would match the docs, but that is breaking.

---

## Issue 2 — the "Never include main.yml from a role" guard can never fire

**Component:** `lib/ansible/plugins/action/include_vars.py`

**Summary.** `_load_files_in_dir` contains a guard meant to stop `include_vars: dir=...`
inside a role from re-loading the role's own `vars/main.yml` (which the role has already
loaded automatically). The comparison joins its fragments to the wrong bases, so the two
sides can never be equal and the skip never happens
(`plugins/action/include_vars.py:268-270`):

```python
# Never include main.yml from a role, as that is the default included by the role
if self._task._role:
    if path.join(self._task._role._role_path, filename) == path.join(root_dir, 'vars', 'main.yml'):
```

Walking `dir: vars` in a role at `/repo/roles/db`, checking `filename = main.yml`:

```
left  = /repo/roles/db/main.yml            (role_path + basename — missing vars/)
right = /repo/roles/db/vars/vars/main.yml  (walked dir + vars/main.yml — vars/ twice)
```

The file actually being considered is `/repo/roles/db/vars/main.yml`; neither side is that
path. The intended comparison appears to be:

```python
if path.join(root_dir, filename) == path.join(self._task._role._role_path, 'vars', 'main.yml'):
```

**Reproduction.**

```
roles/r/vars/main.yml    from_main: 1
roles/r/vars/other.yml   from_other: 2
roles/r/tasks/main.yml   - include_vars: { dir: vars }
                         - debug: { var: from_main }
```

**Actual output (2.21.2):** `"from_main": 1` — the file the comment promises to skip is
loaded.

**Impact.** Not just a double load — a **precedence promotion**. Role vars sit below block
and task vars; `include_vars` results sit above them. Re-loading `main.yml` re-registers
its values at the higher layer, so task-level overrides that beat them before silently lose
after. Live-verified (2.21.2), same task before and after `include_vars: { dir: vars }` with
`vars/main.yml` containing `x: role_val`:

```yaml
- debug: { var: x }
  vars: { x: task_val }
# before: x == task_val    after: x == role_val — the identical override now loses
```

Lesser effects: `name:` duplicates `main.yml`'s keys under the namespace alongside their
role-level copies, and `hash_behaviour` applies a second time.

**Note for maintainers.** Fixing the comparison is a behavior change — nine years of
playbooks have run with `main.yml` included. The alternative is deleting the dead guard and
the comment, making the actual behavior intentional.

---

## Issue 3 — in a role, a missing `dir: vars/...` silently walks the **cwd** and loads contents from the **playbook dir**

**Component:** `lib/ansible/plugins/action/include_vars.py`

**Summary.** `_set_root_dir` resolves a role-relative `dir:` that starts with `vars/` only
when the joined path exists (`plugins/action/include_vars.py:159-164`):

```python
if self.source_dir.split('/')[0] == 'vars':
    path_to_use = (
        path.join(self._task._role._role_path, self.source_dir)
    )
    if path.exists(path_to_use):
        self.source_dir = path_to_use
```

When `<role>/<dir>` does *not* exist there is no error and no fallback assignment —
`source_dir` just stays the relative string, and every later use resolves it against a
*different* base:

- the existence check (`:108`) and the directory walk (`_traverse_dir_depth`) resolve it
  against the **process cwd**
- the per-file loads go through the `DataLoader`, whose basedir is the **playbook dir**
  (`_load_files`)

So the cwd's directory listing decides *which* filenames load, while the playbook dir
supplies their *contents*. Nothing in the docs ("relative to the role or playbook") predicts
any of the three observable outcomes below.

**Reproduction.** Role `r` has no `vars/env`; the playbook dir does; a separate directory
`elsewhere/` holds a same-named tree:

```
repro/play.yml                       - hosts: localhost
                                       gather_facts: false
                                       roles: [r]
repro/roles/r/tasks/main.yml         - include_vars: { dir: vars/env }
                                     - debug: ...
repro/roles/r/vars/main.yml          placeholder: 1
repro/vars/env/leak.yml              marker: loaded_from_playbook_dir
repro/vars/env/only_in_playbookdir.yml   marker2: also_playbook_dir
elsewhere/vars/env/leak.yml          marker: loaded_from_cwd
```

**Actual output (2.21.2)** — the same task, three results chosen by the shell's cwd:

1. **cwd = `elsewhere/`** (cwd has `vars/env/leak.yml`): task **succeeds**, and

   ```
   marker=loaded_from_playbook_dir marker2=UNDEF files=['vars/env/leak.yml']
   ```

   The file list came from the cwd walk (`leak.yml` only — `only_in_playbookdir.yml` is
   skipped although it sits in the directory the contents were read from), the value came
   from the playbook dir, and `ansible_included_var_files` reports a still-relative path.

2. **cwd = any directory without `vars/env`**: `"vars/env directory does not exist"` — even
   though the playbook dir has the directory and both files.

3. **cwd has a file the playbook dir lacks** (delete `repro/vars/env/`): `"Could not find or
   access '<playbook_dir>/vars/env/leak.yml'"` — an error naming a path that was never
   walked.

**Expected.** Either a role-scoped error ("`<role>/vars/env` does not exist"), or a
documented fallback that uses **one** base for both the walk and the load.

**Suggested fix.** Fail when the role-relative dir is missing — the fallback is
undocumented, and because filenames and contents come from two different trees it is hard
to construct a playbook that depends on it *correctly*: any run that "works" through this
path is loading a file list from one directory and values from another. If compatibility
still forbids that, resolving the walk from the loader's basedir (matching the load) at
least makes the two halves agree.
