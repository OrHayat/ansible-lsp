# Upstream issues to file against ansible/ansible — two code paths that can never run

Not filed yet. Read from ansible-core **2.22.0.dev0** (`lib/ansible/release.py:20`).

Both are unreachable code rather than wrong behaviour, which makes them cheap to confirm and
easy to sit unnoticed: nothing fails, a feature simply is not there.

---

## Issue 1 — `_canonicalize_meta` is a no-op, so `meta/runtime.yml` has no validation at all

**Component:** `lib/ansible/utils/collection_loader/_collection_finder.py`

**Summary.** Every `meta/runtime.yml` lookup is a plain `.get()` chain against the raw parsed
dict. The function that was meant to normalise that structure is empty
(`_collection_finder.py:749-765`) — its body is commented out, with a note that relative
redirects were never implemented.

Consequence: **no key in `meta/runtime.yml` is ever validated.** A misspelling is not a
warning, not a `-vvv` line — the entry simply never applies:

- `plugin_rounting:` — silently ignored
- `plugin_routing: {module: ...}` — singular, where the key is `modules`; silently ignored
- any unknown plugin-type key under `plugin_routing` — silently ignored

The file is read once at collection package import (`:705-736`), and a missing file is silent
by design (`:725-726`). It is parsed with `yaml.CBaseLoader`, so every leaf is a string —
`removal_version: 4.0` becomes `"4.0"` (`_collection_meta.py:31-33`).

A schema **does** exist, but only inside `ansible-test`, as a contributor-run sanity check:
`test/lib/ansible_test/_util/controller/sanity/code-smell/runtime-metadata.py` — valid plugin
types at `:240-260`, `import_redirection` allowing only `redirect` at `:264-269`,
`action_groups` metadata allowing only `extend_group` at `:275-287`. A collection author who
does not run `ansible-test` on their own collection gets nothing, and neither does the
consumer installing it.

**Why it matters.** The first symptom of a typo here is a module that cannot be found, or a
deprecation that never warns, with nothing pointing at the metadata file that caused it.

**Expected.** Validate `meta/runtime.yml` keys at load and warn on unknown ones — the
`ansible-test` sanity check already encodes the schema, so it is a matter of moving it to
where end users are — or delete `_canonicalize_meta` and its call site so the dead path stops
implying validation happens.

---

## Issue 2 — the role branch of `path_dwim_relative_stack` is unreachable from all four callers

**Component:** `lib/ansible/parsing/dataloader.py`

**Summary.** `path_dwim_relative_stack` has a branch intended to redirect lookups made from
inside a role's `tasks/` directory (`dataloader.py:365-368`):

```python
if os.path.dirname(unfrackpath(path, follow=False)).endswith('/tasks'):
    ...
```

The test is on the search-path *entry*, and every caller supplies directories, not files:
`get_search_path()` yields `role._role_path` values and `dirname(task file)`
(`playbook/base.py:767-784`). `dirname()` of a directory like `<role>/tasks` is `<role>`,
which does not end in `/tasks`, so the branch never fires — for any of the four call sites,
including `_find_needle` (`plugins/action/__init__.py:1540-1551`).

Two secondary problems in the same two lines:

- `'/tasks'` is hardcoded rather than using `os.sep`, so the test could not fire on Windows
  even if the input shape were right.
- Because the branch is dead, `include_vars: file=x.yml` from a task inside a role does **not**
  search that role's own `vars/` directory — which is the behaviour most users assume it has.
  The effective search is `<role>/tasks/vars/x.yml`, `<role>/tasks/x.yml`,
  `<playbook_dir>/vars/x.yml`, `<playbook_dir>/x.yml`.

**Expected.** Either fix the condition so the intended role-relative lookup works — which
would be a behaviour change worth a changelog entry, since `<role>/vars/` would start
resolving — or remove the branch and document the real search order. The current state is the
worst of both: the code says roles are special-cased, and they are not.

**Confirming it.** Add a `display.warning` inside the branch and run any playbook that uses
`include_vars` with a relative `file:` from within a role; it will not print.

---

## Our side

`T-119` covers the editor-side `meta/runtime.yml` schema check. `T-097` covers `include_vars`
candidate paths — and Issue 2 is precisely *why* the role's own `vars/` must not be in that
list, so that ticket cites this file rather than re-deriving it.
