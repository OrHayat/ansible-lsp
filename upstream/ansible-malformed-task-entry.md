# Upstream issue to file against ansible/ansible — an internal assertion is on a user-reachable path, so a non-dict task entry reports the wrong value, the wrong type, and no location

Not filed yet. Measured on ansible-core **2.21.2** (`lib/ansible/release.py:20`); the code is
two lines and long-standing.

## Issue 1 — `load_list_of_tasks` raises a developer assertion at the user, naming the list where it means the entry

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

**Why it reads like debug output: it is.** `AnsibleAssertionError` is
`"""Invalid assertion."""` (`errors/__init__.py:183`), inheriting Python's `AssertionError` —
an internal "this cannot happen" class. Of its **24** raise sites across `lib/ansible`,
**none** passes `obj=`. It was never meant to reach a user, so nobody gave it a position or a
sentence. This particular guard just happens to sit where ordinary input arrives: a string in a
task list is valid YAML that a human writes by accident.

Someone did notice it leaks. `Block._load` catches `AssertionError` and re-raises it wrapped in
a human sentence (`block.py:113-124`) — the `A malformed block was encountered while loading
block` prefix. That wrap treats the symptom and leaves the assertion as it is.

**Why `Origin: <unknown>` survives the wrap.** The wrapper *does* pass `obj=self._ds`. But for
a bare task list `preprocess_data` synthesises the block it is loading —
`dict(block=ds)` (`block.py:107-110`) — and that fresh dict carries no YAML origin tag, so
there is nothing to report a position from. The only object here that knows where it came from
is `task_ds` itself, and it is the one object the raise does not mention.

**Expected.** Name the entry that is wrong, its real type, and where it is.

**Suggested fix.** Make it a parser error rather than an assertion, so it carries its own
position and is not swallowed by the `AssertionError` handler above it:

```python
     for task_ds in ds:
         if not isinstance(task_ds, dict):
-            raise AnsibleAssertionError('The ds (%s) should be a dict but was a %s' % (ds, type(ds)))
+            raise AnsibleParserError(
+                'A task must be a dict, but was a %s' % type(task_ds).__name__,
+                obj=task_ds,
+            )
```

`AnsibleParserError` is what every neighbouring user-facing raise in this file already uses
(`helpers.py:106`, `154`, `247`), and it is deliberately *not* an `AssertionError`, so
`Block._load` stops catching it and the error propagates with the origin of the offending entry
instead of the synthesised block's absence of one.

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
