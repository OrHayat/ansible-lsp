# Upstream issue to file against ansible/ansible — a static import loop is reported as "probably a bug", with no file named

Not filed yet. Measured on ansible-core **2.21.3** (`lib/ansible/release.py:20`) under Python
3.14. The role half of the same gap is open upstream as
[#61527](https://github.com/ansible/ansible/issues/61527) — "Role dep cycle detection broken on
Py3.7+", filed 2019 — and the `TODO` above the role loader's catch still points at it. A search
of the tracker found nothing for the tasks and playbook forms, only deep-chain reports.

## Issue 1 — an `import_tasks` or `import_playbook` loop escapes as a RecursionError, so the user is told it is an ansible bug and given no file

**Component:** `lib/ansible/playbook/helpers.py`, `lib/ansible/playbook/playbook_include.py`

**Summary.** The static forms are expanded while the play loads, by plain recursion:
`load_list_of_tasks` → `load_list_of_blocks` → `load_list_of_tasks` for `import_tasks`
(`helpers.py:209-224`), and `Playbook._load_playbook_data` → `PlaybookInclude.load_data` →
`Playbook._load_playbook_data` for `import_playbook` (`playbook/__init__.py:94`,
`playbook_include.py:108`). Neither path remembers which files are already being loaded. A file
that imports itself, directly or through another file, recurses until Python's limit, and the
`RecursionError` reaches the CLI's catch-all (`cli/__init__.py:670-672`), which is written for
genuine internal faults:

```python
except Exception as ex:
    try:
        raise AnsibleError("Unexpected Exception, this is probably a bug.") from ex
```

So the author reads "this is probably a bug", gets a 999-frame traceback, and is never told
which file or line closes the loop. Exit code 250. `--syntax-check` fails identically.

**Reproduction:**

```yaml
# repro.yml
- hosts: localhost
  gather_facts: false
  tasks:
    - import_tasks: loop.yml
```

```yaml
# loop.yml
- import_tasks: loop.yml
```

```
$ ansible-playbook -i localhost, repro.yml
[ERROR]: Unexpected Exception, this is probably a bug: maximum recursion depth exceeded
Traceback (most recent call last):
  ...
  File ".../ansible/playbook/helpers.py", line 224, in load_list_of_tasks
  File ".../ansible/playbook/helpers.py", line 67, in load_list_of_blocks
  File ".../ansible/playbook/block.py", line 88, in load
  ...  (the same frames, 123 times over)
RecursionError: maximum recursion depth exceeded
$ echo $?
250
```

The playbook form is one file:

```yaml
# self.yml
- import_playbook: self.yml
```

```
$ ansible-playbook -i localhost, self.yml
[ERROR]: Unexpected Exception, this is probably a bug: maximum recursion depth exceeded
  ...
  File ".../ansible/playbook/__init__.py", line 94, in _load_playbook_data
  File ".../ansible/playbook/playbook_include.py", line 54, in load
  File ".../ansible/playbook/playbook_include.py", line 108, in load_data
  ...  (326 times over)
```

Self-contained, for filing:

```sh
d=$(mktemp -d) && cd "$d"
printf -- '- hosts: localhost\n  gather_facts: false\n  tasks:\n    - import_tasks: loop.yml\n' > repro.yml
printf -- '- import_tasks: loop.yml\n' > loop.yml
printf -- '- import_playbook: self.yml\n' > self.yml
for pb in repro.yml self.yml; do
  ansible-playbook -i localhost, "$pb" > "$pb.log" 2>&1; echo "$pb: exit $?"; head -1 "$pb.log"
done
```

**Measured scope.**

- A two-file loop, `a.yml` importing `b.yml` importing `a.yml`: the same crash.
- `when: false` on every edge of the loop: the same crash. A static import is expanded before
  any condition exists, so the guard cannot break it. Control: the identical loop written with
  `include_tasks` and `when: false` runs clean (`ok=1 skipped=1`), so the probe distinguishes.
- An `import_role` loop, and a `meta/main.yml` dependency cycle, get a real error: "A recursion
  loop was detected with the roles specified. Make sure child roles do not have dependencies on
  parent roles", with an `Origin:` line at the closing edge. That is not detection either —
  `Role.load` catches the same `RecursionError` and rewraps it with `obj=role_include._ds`
  (`role/__init__.py:230-232`), under a `TODO` that says cycle detection needs fixing and cites
  #61527 — but it gives the author a sentence and a line, which the tasks and playbook forms do
  not. The catch lives in `Role.load`, and a task or playbook import never passes through it.
- A mixed loop, one `import_tasks` edge and one `include_tasks` edge, only closes at runtime:
  124 includes, then the same crash. Dynamic recursion can be legitimate behind a counter, so
  that case is not this issue's subject.

**Expected.** A parser error at the entry that closes the loop, naming the chain:

```
[ERROR]: import loop: a.yml -> b.yml -> a.yml
Origin: b.yml:1:3
```

**Suggested fix.** Both loaders already have the chain in hand; neither compares against it.

For `import_tasks`, `load_list_of_tasks` walks the enclosing includes through `_parent` to
build the search path (`helpers.py:179-201`), visiting every ancestor `TaskInclude`. The
resolved `include_file` is known at `helpers.py:209` and the copy that becomes the nested
loads' parent is built at `helpers.py:222-224`. Recording the canonical `include_file` on that
copy, and raising `AnsibleParserError(..., obj=task_ds)` when the ancestor walk meets the file
about to be loaded, turns the crash into a positioned error using the walk that already runs.

For `import_playbook`, `PlaybookInclude.load_data` has the absolute playbook path before it
recurses (`playbook_include.py:88-108`). A tuple of in-flight playbook paths carried through
`_load_playbook_data`, checked before the call at line 108, gives the same error with
`obj=` the import entry, which carries its origin.

## Our side

Planned as the static tier of T-022 (`tasks/open/T-022-circular-include.md`): every loop
whose edges are all `import_tasks`, `import_playbook`, `import_role` or `meta/main.yml`
dependencies is an ERROR at the closing edge, with the full path in the message. For the two
forms here ours will be the only pointer the author gets. The rule follows literal edges only
and stays silent on a loop that exists only through a templated candidate, so it cannot fire
on correct code. `demo/roles/cycle-a` and `cycle-b` already hold the meta form of the fixture.
