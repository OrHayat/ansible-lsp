# Upstream issue to file against ansible/ansible — a non-dict task entry reports the wrong value, the wrong type, and no location

Not filed yet. Measured on ansible-core **2.21.2** (`lib/ansible/release.py:20`); the code is
two lines and long-standing.

## Issue 1 — `load_list_of_tasks` interpolates the list where it means the item, and raises with no `obj`

**Component:** `lib/ansible/playbook/helpers.py`

**Summary.** Every entry of a task list must be a mapping. The guard is correct; its message is
not (`helpers.py:100-102`):

```python
for task_ds in ds:
    if not isinstance(task_ds, dict):
        raise AnsibleAssertionError('The ds (%s) should be a dict but was a %s' % (ds, type(ds)))
```

`ds` is the **whole list**. `task_ds` is the offending entry. Both interpolations use `ds`, so:

- the reported type is always `<class 'list'>`, whatever the bad entry actually is;
- the reported value is every sibling in the list, including the valid ones;
- and because the raise carries no `obj=`, ansible cannot attach a file or line, so the error
  prints `Origin: <unknown>`.

**Reproduction:**

```yaml
# repro.yml
- hosts: localhost
  gather_facts: false
  tasks:
    - just a string
    - another one
```

```
$ ansible-playbook --syntax-check repro.yml
[ERROR]: A malformed block was encountered while loading block:
         The ds (['just a string', 'another one']) should be a dict but was a <class 'list'>
Origin: <unknown>
```

The author wrote two bad entries in one file and is told the type of the container, shown both
entries as one blob, and given no line to go to. On a real task file of any size the value is a
wall of YAML with the actual mistake somewhere inside it.

The wrapping text — `A malformed block was encountered while loading block` — comes from
`Block._load` catching the `AssertionError` (`block.py:113-124`), which is why the word "block"
appears even when the entry is in a plain `tasks:` list.

**Measured scope.** Fatal in every task list: a play's `tasks:`, `pre_tasks:`, `post_tasks:`,
`handlers:`, inside a block's `block:`/`rescue:`/`always:`, and a role's `tasks/main.yml`. A
**null** entry (a bare `-`) is not affected — `load_list_of_blocks` drops it before this point
(`helpers.py:53,65`), and the play loads clean.

**Expected.** Name the entry that is wrong, its real type, and where it is.

**Suggested fix.** Two tokens and an `obj`:

```python
     for task_ds in ds:
         if not isinstance(task_ds, dict):
-            raise AnsibleAssertionError('The ds (%s) should be a dict but was a %s' % (ds, type(ds)))
+            raise AnsibleAssertionError(
+                'The task (%s) should be a dict but was a %s' % (task_ds, type(task_ds)),
+                obj=task_ds,
+            )
```

`obj=task_ds` is what restores the `Origin:` line — the same mechanism every neighbouring raise
in this file already uses (`helpers.py:106`, `helpers.py:154`, `helpers.py:247`).

Worth checking the sibling at `helpers.py:96-97` in the same pass: `The ds (%s) should be a list
but was a %s` has the same shape and is correct there, since `ds` really is the subject — which
is likely how the copy at line 102 went unnoticed.

## Our side

Shipped as `malformed-task-entry`, a rule id of ours rather than a replication, because there is
no usable upstream message to borrow: the type is wrong, the value is the wrong object, and
there is no position. Ours names the entry and anchors on it, which is the part ansible cannot
do at all until `obj=` is added.

The null and alias cases are excluded deliberately. A bare `-` is measured clean upstream. An
alias resolves to whatever the anchor holds, so it may well be a mapping — judging it needs the
substitution T-160 adds, and until then it stays silent rather than guessing.

If upstream takes the fix, our message stays the more precise of the two, and our rule id keeps
it suppressible independently of the rules that mirror ansible verbatim.
