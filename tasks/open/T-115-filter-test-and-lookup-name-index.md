# T-115 — Filter, test and lookup name index

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-114 | —          |

## Problem

A misspelled filter or test is a run-time error, and a `with_typo:` is a **fatal** parse
error — `with_X` is recognised only when `X` is a loaded lookup plugin name (`task.py:336`),
otherwise the key falls through to the unknown-keyword path. None of the three names is
checked or completed today.

Counts, taken from the registration dicts rather than the `.yml` doc files — the docs
undercount, because aliases and the Jinja overrides ship without their own doc:

| Kind | Names | Source |
| ---- | ----- | ------ |
| Filters | **78** | `filter/core.py:734-834` (58), `mathstuff.py:212` (16), `encryption.py:72` (2), `urls.py:16` (1), `urlsplit.py:82` (1) |
| Tests | **51** | `test/core.py:302` (27), `files.py:26` (14), `mathstuff.py:49` (7), `uri.py:35` (3) |
| Lookups | **25** | `plugins/lookup/*.py` |

Aliasing is heavy: `failed`/`failure`, `succeeded`/`success`/`successful`, `changed`/`change`,
`skipped`/`skip`, `version`/`version_compare`, `subset`/`issubset`, `nan`/`isnan`,
`directory`/`is_dir`, `abs`/`is_abs`, `same_file`/`is_same_file`, `mount`/`is_mount`,
`d`/`default`. An index keyed on doc filenames misses roughly a third of the legal spellings.

## The trap

Eight entries in `core.py` are marked *"Jinja builtins that need special arg handling"* —
`d`, `default`, `map`, `select`, `selectattr`, `reject`, `rejectattr`, `groupby` — and they
**replace** Jinja's implementations rather than wrapping them. Which means:

> Ansible's 78 filters are **not** the legal set. Every stock Jinja2 filter (`upper`, `join`,
> `length`, `int`, `replace`, `sort`, ...) is legal too. An "unknown filter" diagnostic built
> from the Ansible list alone false-positives on all of them.

So this needs a hardcoded Jinja2 builtin list unioned in, and that drags in a Jinja-version
dependency nothing else in this codebase has. That cost is the reason for the ordering below.

## Approach

**Completion first, diagnostic second.** Completion degrades gracefully when the list is
incomplete — a missing name is a missing suggestion. A diagnostic does not: a missing name is
a false error on working code. Ship the index and completion, live with it, and only add the
unknown-name rule once the union is trusted.

Collection-provided filters/tests/lookups come from the same plugin dirs and are subject to
`meta/runtime.yml` routing, so the diagnostic half also wants T-064.

## Done when

- [ ] the three name sets are generated from the checkout, not hand-typed, with the
      ansible-core version recorded
- [ ] aliases are all present
- [ ] Jinja2's own builtins are in the union, with the Jinja version recorded
- [ ] completion works for filters, tests and `with_*`
- [ ] the unknown-name diagnostic is a separate, later decision — not shipped with the index
