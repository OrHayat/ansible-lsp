# Upstream issue to file against ansible/ansible — `duplicate loop in task` depends on which keyword was written first

Not filed yet. Measured on ansible-core **2.21.2** (`lib/ansible/release.py:20`); the code
below is long-standing and not a recent regression.

## Issue 1 — `loop:` and `with_<lookup>` on one task is fatal in one order and silently mis-executed in the other

**Component:** `lib/ansible/playbook/task.py`

**Summary.** `_preprocess_with_loop` refuses a second loop by checking whether one is
*already* recorded (`task.py:252-261`):

```python
def _preprocess_with_loop(self, ds, new_ds, k, v):
    loop_name = k.removeprefix("with_")
    if new_ds.get('loop') is not None or new_ds.get('loop_with') is not None:
        raise AnsibleError("duplicate loop in task: %s" % loop_name, obj=ds)
    if v is None:
        raise AnsibleError("you must specify a value when using %s" % k, obj=ds)
    new_ds['loop_with'] = loop_name
    new_ds['loop'] = v
```

`preprocess_data` walks `ds.items()` in written order (`task.py:332-342`), and a plain
`loop:` key is not routed through that function — it lands in the generic branch,
`new_ds[k] = v`. So the guard only sees a conflict when the `with_*` comes **second**:

| Written order                | Result                                              |
| ---------------------------- | --------------------------------------------------- |
| `loop:` then `with_items:`   | `AnsibleError: duplicate loop in task: items`        |
| `with_items:` then `loop:`   | runs clean                                           |
| `with_items:` then `with_list:` | `duplicate loop in task: list`                    |
| `with_list:` then `with_items:` | `duplicate loop in task: items`                   |

Two `with_*` keys are symmetric, because both go through the function. Mixing `loop:` with a
`with_*` is not.

**The accepted order does not simply ignore the loser.** `_preprocess_with_loop` writes
*two* keys — `loop_with` and `loop`. A later `loop:` overwrites only `loop`; `loop_with`
survives, and at run time it selects the lookup applied to whatever `loop` now holds
(`task_executor.py:157-170`). The discarded keyword's **value** is dropped while its
**lookup** is retained, so the surviving `loop:` is evaluated under a plugin the author
never asked for.

**Reproduction** (any recent ansible-core):

```yaml
# repro.yml
- hosts: localhost
  gather_facts: false
  tasks:
    - name: the with_ value is discarded, but its lookup is not
      ansible.builtin.debug:
        msg: "{{ item }}"
      with_together: [[1], [2]]
      loop: [[a, b], [c, d]]

    - name: the same loop, written alone
      ansible.builtin.debug:
        msg: "{{ item }}"
      loop: [[a, b], [c, d]]
```

```
TASK [the with_ value is discarded, but its lookup is not]
ok: [localhost] => (item=['a', 'c'])      <- zipped by the discarded with_together
ok: [localhost] => (item=['b', 'd'])

TASK [the same loop, written alone]
ok: [localhost] => (item=['a', 'b'])
ok: [localhost] => (item=['c', 'd'])
```

Same `loop:` value, different iterations, no error and no warning. `with_items` produces the
same shape of surprise more quietly, since its lookup only flattens a level.

**Why this matters.** Both spellings on one task are a mistake in either order — most often a
half-finished migration from `with_items:` to `loop:`, where the old line was left above the
new one. That is precisely the order Ansible accepts. The author gets no error, no warning,
and a loop whose iteration count and item shape are decided by the line they thought they had
replaced. Reordering two keys in a YAML mapping — something a formatter or a merge may do —
flips the task between fatal and silently wrong.

**Expected.** The conflict is a property of the task, not of key order. Either raise
`duplicate loop in task` whenever both a `loop:` and a `with_*` are present, or — at minimum
— clear `loop_with` when `loop` is overwritten, so the surviving keyword means what it says.

**Suggested fix.** Detect the pair up front, before the ordered walk:

```python
def preprocess_data(self, ds):
    ...
    with_keys = [k for k in ds if k.startswith('with_') and k.removeprefix('with_') in lookup_loader]
    if with_keys and ds.get('loop') is not None:
        raise AnsibleError("duplicate loop in task: %s" % with_keys[0].removeprefix('with_'), obj=ds)
```

This makes playbooks that currently run start failing, which is the point — they are running
a loop the author did not write. A deprecation cycle warning on the accepted order would be
the gentler path.

## Related

- `with_<lookup>` is only treated as a loop when the suffix names an **installed** lookup
  (`task.py:336`). So `with_item` — the obvious typo for `with_items` — is not a loop at all;
  it falls through to `'with_item' is not a valid attribute for a Task`. Combined with the
  above, `with_item:` followed by `loop:` is silently just a `loop:`.
- A `loop:` written with no value never counts as a loop, since the guard is `is not None`.
  `loop:` followed by `with_items: [a]` therefore runs, using the `with_items` value.

## Our side

`T-110` row 10 replicates Ansible's own error, with Ansible's message, for the fatal order.

The accepted order gets a warning of **ours**, on its own rule id `shadowed-loop`, rather
than a borrowed error — a decision taken knowingly: this is the one place `placement.rs`
speaks where ansible-core is silent. It is a WARNING, not an error, because the playbook does
run; it just runs a loop nobody wrote. The message names the discarded keyword, the lookup
that survived it, and the fix. Separate id so it can be suppressed and toggled without
touching the rules that mirror upstream.

If upstream takes the fix, our warning becomes redundant for new cores and stays useful for
old ones — the same shape as every other version-sensitive rule here.
