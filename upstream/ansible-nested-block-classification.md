# Upstream issue to file against ansible/ansible — a mis-indented `rescue:`/`always:` gets a good error at the top of a task list and an internal class name one level down

Not filed yet. Measured on ansible-core **2.21.2** (`lib/ansible/release.py:20`); both code
paths below are long-standing and not a recent regression.

## Issue 1 — two task-list walkers disagree about what counts as a block

**Component:** `lib/ansible/playbook/helpers.py`

**Summary.** Ansible has two functions that walk a list of tasks, and they use two different
tests for "is this entry a block?".

`load_list_of_blocks` uses `Block.is_block`, which accepts **any** of the three keys
(`helpers.py:53`, `block.py:91-98`):

```python
while block_ds is not None and not Block.is_block(block_ds):   # helpers.py:53
    ...

@staticmethod
def is_block(ds):                                              # block.py:91-98
    is_block = False
    if isinstance(ds, dict):
        for attr in ('block', 'rescue', 'always'):
            if attr in ds:
                is_block = True
                break
    return is_block
```

`load_list_of_tasks` tests only `block` (`helpers.py:104`):

```python
for task_ds in ds:
    ...
    if 'block' in task_ds:                                     # helpers.py:104
        if use_handlers:
            raise AnsibleParserError("Using a block as a handler is not supported.", obj=task_ds)
        task = Block.load(task_ds, ...)
    else:
        args_parser = ModuleArgsParser(task_ds)
        ...
```

The two hand off to each other, and the lenient test runs exactly once per task list — at the
outermost level. `Block._load` loads a block's `block:`/`rescue:`/`always:` contents through
`load_list_of_tasks` (`block.py:113-124`), and from there the recursion stays in the strict
walker:

```
Play._load_tasks
 └─ load_list_of_blocks(ds)              helpers.py:31  — Block.is_block  (any of three)
     └─ Block.load(entry)
         └─ Block._load(attr, ds)        block.py:113
             └─ load_list_of_tasks(ds)   helpers.py:89  — 'block' in ds  (one of three)
                 └─ Block.load(entry)  ─┐
                     └─ Block._load ────┘  …strict from here down, at every depth
```

So a mapping carrying a `rescue:` with no `block:` is recognised as a malformed **block** at
the top of a task list, and mistaken for a **task** anywhere below it. In the second case it
reaches `ModuleArgsParser`, which takes `rescue` for a module name, finds a list where module
args should be, and raises with the repr of an internal type (`mod_args.py:253`):

```python
raise AnsibleParserError("unexpected parameter type in action: %s" % type(thing), obj=self._task_ds)
```

**Measured.** The same three lines, differing only in indentation:

| Where the `rescue:` sits            | Error                                                                                     |
| ----------------------------------- | ----------------------------------------------------------------------------------------- |
| top level of a play's `tasks:`      | `'rescue' keyword cannot be used without 'block'`                                          |
| top level of an `include_tasks` target | `'rescue' keyword cannot be used without 'block'`                                       |
| nested in a `block:` list           | `unexpected parameter type in action: <class '...._datatag._AnsibleTaggedList'>`           |
| nested in a `rescue:` list          | `unexpected parameter type in action: <class '...._datatag._AnsibleTaggedList'>`           |

`always:` behaves identically to `rescue:` in all four. Every case exits **4**, under both
`ansible-playbook` and `--syntax-check` — the verdict is right in all of them, and nothing
runs. Only the explanation differs.

**Reproduction:**

```yaml
# good-error.yml — rescue: at the top of the task list
- hosts: localhost
  gather_facts: false
  tasks:
    - rescue:
        - debug: {msg: x}
```

```
[ERROR]: 'rescue' keyword cannot be used without 'block'
```

```yaml
# bad-error.yml — the same mapping, one level deeper
- hosts: localhost
  gather_facts: false
  tasks:
    - block:
        - rescue:
            - debug: {msg: x}
```

```
[ERROR]: unexpected parameter type in action: <class 'ansible.module_utils._internal._datatag._AnsibleTaggedList'>
Origin: bad-error.yml:5:11
```

**Why this matters.** The nested spelling is not an exotic shape — it is a one-level
mis-indent of an ordinary `block`/`rescue` pair, which is what the correct form looks like
with `rescue:` slid two columns right:

```yaml
- block:                  # correct: rescue is a sibling key
    - debug: {msg: try}
  rescue:
    - debug: {msg: catch}

- block:                  # mis-indented: rescue is now an item of the block's list
    - debug: {msg: try}
    rescue:
      - debug: {msg: catch}
```

Ansible already knows how to name this fault, and does so when the same mapping appears one
indent to the left. In the nested position the author instead gets the repr of
`ansible.module_utils._internal._datatag._AnsibleTaggedList` — a private, underscore-prefixed
type that appears nowhere in the docs and says nothing about the keyword they mistyped. The
`Origin:` line points at the right row, so the user knows *where* but not *what*.

**Expected.** The classification of an entry should not depend on how deeply the task list is
nested. A mapping with `rescue:` or `always:` and no `block:` is a malformed block wherever it
appears, and should get `'%s' keyword cannot be used without 'block'` at every depth.

**Suggested fix.** Have the inner walker use the same predicate as the outer one
(`helpers.py:104`):

```python
-        if 'block' in task_ds:
+        if Block.is_block(task_ds):
```

`Block.load` handles the rest: `preprocess_data` leaves an already-block mapping alone
(`block.py:100-112`), and `_validate_rescue`/`_validate_always` produce the message
(`block.py:138-142`). Every entry this moves is fatal today and stays fatal — the diagnosis
improves and the verdict does not.

The one shape it would newly reject is a bare call to a module actually *named* `rescue` or
`always` from a nested position. That name is already unusable at the top level of any task
list, where `is_block` claims it first, so the fix makes an existing restriction consistent
rather than adding one.

One deliberate consequence worth calling out in the PR: a nested `rescue:`-only mapping inside
`handlers:` would begin hitting the `Using a block as a handler is not supported.` raise on the
line above, rather than the `mod_args` error. That is also a better message for that shape, but
it is a message change, not just a text improvement.

## Related

- `Block.is_block` is used in three places (`block.py:86`, `block.py:106`, `helpers.py:53`) and
  `'block' in ds` in one (`helpers.py:104`). The odd one out is the inner walker.
- A **null** value for any of the three is a third message again —
  `A malformed block was encountered while loading rescue.` — raised when `load_list_of_tasks`
  gets `None` and `Block._load` catches the `AssertionError` (`block.py:113-124`). That one is
  clear enough and is not part of this report.
- The guard that produces the good message is `if value and not self.block`
  (`block.py:138-142`), Python truthiness on both sides: an empty `block: []` counts as no
  block and still raises, while an empty `rescue: []` raises nothing at all.

## Our side

`T-110` row 7 ships the good message, and `placement.rs` classifies with `is_block`'s
any-of-three at **every** depth — so we give
`'rescue' keyword cannot be used without 'block'` in both positions rather than replicate the
depth-dependent behaviour. Pinned by `a_nested_rescue_only_mapping_is_a_block_too` in
`ast.rs`.

That is a divergence from upstream's *message* in the nested position, but not from its
verdict, so it stays on the `invalid-placement` rule id with Ansible's own wording rather than
becoming a rule of ours. Inventing a nested-only message — "did you mean to align this with the
`block:` above?" — was considered and rejected: it guesses at intent (a leftover `rescue:` from
a deleted block reads identically), and making our text depend on nesting depth would reproduce
the exact disease this report is about.

Getting there also required fixing the same bug on our side: `ast::build_stmt` and
`placement::stmt` both tested `block:` alone, which sent a bare `rescue:` to `find_action`,
where the keyword was taken for the module name — leaving `unknown_keys` empty and every rule
downstream silent. See T-110's Approach for the measured before/after.

If upstream takes the one-line fix, our behaviour is already what it would produce.
