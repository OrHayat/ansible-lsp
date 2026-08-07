# T-093 — Bare module names only try .py

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | S    | T-090 | —          |

## Symptom

Go-to-definition on a bare module name silently fails for every custom module that is not
Python. `library/mymod` (extensionless), `library/mymod.ps1` and `library/mymod.sh` all run
under Ansible; none resolve here.

## Cause

`resolve.rs:502` builds the candidate as `format!("{bare}.py")`. Ansible's `module_loader` is
constructed with `class_name=''`, so its suffix is `''` (`plugins/loader.py:780-788`, `:1799`)
and the path cache is keyed under both the empty and the real extension (`:899-927`) — any
extension, or none, is a match.

~~`.py` *is* right for the FQCN branch~~ **Corrected during the fix**: the FQCN *module*
finder fuzzy-matches extensions exactly like the legacy one — `_find_fq_plugin` tries the
exact name, then a sorted glob of `name.*` (`loader.py:704-719`), and the module loader
passes no extension. `ansible.windows` ships `.ps1` modules this way, and since 2.14 a
collection may ship them with `.yml` sidecar docs and no `.py` at all. The hard `.py`
suffix belongs only to controller-side *class* plugins (action, lookup, filter — loaders
with a `class_name`, `loader.py:782-784`).

## Fix

One matcher for both branches: `module_files_named` (resolve.rs) reads the directory
through the `Fs` seam and accepts `name` bare or with any one extension —
`splitext(file) == name` (`loader.py:907-908`) — minus `MODULE_IGNORE_EXTS`
(`constants.py:62` + `base.yml:1799-1801`). Matches are sorted, first wins: exactly the
FQCN finder's rule; the legacy finder takes `os.listdir` order, which is
filesystem-arbitrary, so sorted stands in as the deterministic pick there too. Used by the
bare branch (per legacy dir) and the FQCN branch (`plugins/modules/` per collection root);
`plugins/action/` candidates stay `.py`-only. A dir with no match keeps a representative
`name.py` candidate so diagnostic trails still name every place tried.

Fixtures: `demo/library/sweep.sh` and
`demo/collections/.../demo/charlie/plugins/modules/pulse.sh` (bash modules, tasks in
`demo/tasks/modules.yml`), pinned by `demo_non_python_modules_resolve`. Tempdir tests
`legacy_modules_match_any_extension` (`.ps1`, extensionless, `.md` ignored) and
`ambiguous_module_match_takes_sorted_first` (extensionless beats `.ps1` beats `.py`;
losers stay in the trail). All three fail with the old `.py`-only matching (verified by
temporarily reverting the matcher).

## Done when

- [x] `library/mymod.ps1` and extensionless `library/mymod` resolve
- [x] ~~the FQCN branch still requires `.py`~~ the FQCN **modules** branch globs like the
      legacy one (it always did upstream — see Cause); only `plugins/action/` still
      requires `.py`
- [x] a fixture pins the ambiguous-match order (Ansible globs, sorts, takes the first —
      `loader.py:704-719`, `display.debug` only; surfacing that is T-023's business, not ours)
