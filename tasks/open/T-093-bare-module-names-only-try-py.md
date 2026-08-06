# T-093 — Bare module names only try .py

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | S    | T-090 | —          |

## Symptom

Go-to-definition on a bare module name silently fails for every custom module that is not
Python. `library/mymod` (extensionless), `library/mymod.ps1` and `library/mymod.sh` all run
under Ansible; none resolve here.

## Cause

`resolve.rs:502` builds the candidate as `format!("{bare}.py")`. Ansible's `module_loader` is
constructed with `class_name=''`, so its suffix is `''` (`plugins/loader.py:780-788`, `:1799`)
and the path cache is keyed under both the empty and the real extension (`:899-927`) — any
extension, or none, is a match.

`.py` *is* right for the FQCN branch (`resolve.rs:515-532`) and for the action, filter and
lookup loaders, which do pass a `class_name`. Only the bare-name path is wrong.

## Fix

For the legacy/bare branch, glob the directory instead of constructing one filename.

## Done when

- [ ] `library/mymod.ps1` and extensionless `library/mymod` resolve
- [ ] the FQCN branch still requires `.py`
- [ ] a fixture pins the ambiguous-match order (Ansible globs, sorts, takes the first —
      `loader.py:704-719`, `display.debug` only; surfacing that is T-023's business, not ours)
