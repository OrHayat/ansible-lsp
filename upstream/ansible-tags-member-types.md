# Upstream issues to file against ansible/ansible — `tags` declares two legal member types, and the second one has no working use; two member shapes crash outright

Not filed yet. Measured on ansible-core **2.21.2** (`lib/ansible/release.py:20`).

The declaration is one line (`taggable.py:44`):

```python
tags = FieldAttribute(isa='list', default=list, listof=(str, int), extend=True)
```

Three separate problems fall out of it. They are grouped here because they share that line, but
they are independent fixes.

## Issue 1 — an `int` tag is accepted, cannot be selected, and crashes both listing commands

`listof=(str, int)` says an integer tag is legal, and the loader agrees — a play with
`tags: [7]` passes `--syntax-check` and runs. Nothing else about it works.

**Selection silently misses it.** `--tags 7` matches a task tagged `"7"` and not one tagged `7`:

```
$ ansible-playbook -c local -i localhost, --tags 7 int_only.yml    # tags: [7]
(no play recap — nothing selected)

$ ansible-playbook -c local -i localhost, --tags 7 str_only.yml    # tags: ["7"]
localhost : ok=1  changed=0  unreachable=0  failed=0 ...
```

The CLI hands `--tags` through as a string, and the comparison never coerces, so the one thing
a tag exists for cannot be done with an integer one.

**Listing crashes.** Both `--list-tasks` and `--list-tags` join the tag list into a message
without converting (`cli/playbook.py:206` and `:220`):

```
$ ansible-playbook -i localhost, --list-tasks int_only.yml
[ERROR]: Unexpected Exception, this is probably a bug: sequence item 0: expected str instance,
         _AnsibleTaggedInt found
    taskmsg += "\tTAGS: [%s]\n" % ', '.join(cur_tags)
```

So an integer tag is inert for selection and fatal for inspection. Either `listof` should drop
`int`, or the CLI should coerce — but the declaration as written promises something the rest of
the code does not implement.

## Issue 2 — mixing the two declared-legal types crashes `--list-tasks`, while the play itself runs

Worse than issue 1, because both member types are individually blessed. `tags: [deploy, 7]`
passes `--syntax-check`, and `ansible-playbook` runs it to `ok:`. Listing it does not
(`cli/playbook.py:201`):

```python
cur_tags = list(mytags.union(set(task.tags)))
cur_tags.sort()
```

```
$ ansible-playbook -i localhost, --list-tasks mix.yml
[ERROR]: Unexpected Exception, this is probably a bug: '<' not supported between instances of
         '_AnsibleTaggedInt' and '_AnsibleTaggedStr'
```

`extend=True` makes this easy to hit without writing both types in one place: a play tagged
`[release]` and a task tagged `[7]` merge into one list before the sort. Neither file looks
wrong on its own.

The sort exists only to make the output stable. `sort(key=str)` fixes it, and `', '.join(...)`
needs `map(str, ...)` alongside — issue 1's crash is the same statement one line down.

## Issue 3 — the reserved-tag warning prints its names in a different order every run

`taggable.py:58-59`:

```python
if found := self._RESERVED.intersection(tags):
    _display.warning(f"Found reserved tagnames in tags: {list(found)!r}, ...", obj=ds)
```

`found` is a set, and Python randomises string hashing per process, so `list(found)` is a fresh
order each time. Five runs of one unchanged file:

```
Found reserved tagnames in tags: ['tagged', 'all', 'untagged'], ...
Found reserved tagnames in tags: ['all', 'untagged', 'tagged'], ...
Found reserved tagnames in tags: ['tagged', 'untagged', 'all'], ...
Found reserved tagnames in tags: ['all', 'untagged', 'tagged'], ...
Found reserved tagnames in tags: ['untagged', 'all', 'tagged'], ...
```

A diagnostic that changes between identical runs cannot be grepped for, diffed across CI logs,
or asserted in a test. `sorted(found)` is the whole fix.

## Issue 4 — an unhashable member crashes at load, before `listof` ever runs

`tags: [[a]]` or `tags: [{a: b}]` is a list, so `_load_tags` accepts it, and the reserved-name
check on the next line then calls `frozenset.intersection` on it:

```
$ ansible-playbook --syntax-check nested.yml
[ERROR]: Unexpected Exception, this is probably a bug: unhashable type: '_AnsibleTaggedList'
```

The type check that would have rejected it is `listof`, and that runs in `post_validate`
(`base.py:483-486`), which is *later* — at run time, not at load. So the guard that exists for
exactly this input never gets the chance, and the user gets a traceback where a parser error
belongs. The float case shows what the intended message looks like when it does get there:

```
Task failed: Error processing keyword 'tags': Keyword 'tags' items must be of type 'str' or
'int', not 'float'.
```

Hoisting the `listof` check ahead of the reserved-name intersection would turn all of these
into that sentence.

## Our side

T-110 rows 28 and 29 ship the two rules worth replicating: the `tags:` shape (a list or a comma
string, nothing else) and the reserved-name warning. Row 29 is **not** verbatim — issue 3 means
there is no single upstream string to copy, so ours sorts the names and carries its own rule id,
`reserved-tag-name`.

Issues 1, 2 and 4 are candidates for a row of our own: each is a guaranteed failure visible
without running anything, and each is reported upstream as "this is probably a bug", which tells
the author nothing. Not implemented yet — see T-110 row 30.

A known miss on our side, unrelated to any of the above: `tags: 42` is fatal upstream and
`tags: "42"` is fine, and our parser keeps no scalar style, so the two are one node to us. We
stay silent rather than flag the legal spelling — the same trade row 17 takes on `hosts: 42`.
