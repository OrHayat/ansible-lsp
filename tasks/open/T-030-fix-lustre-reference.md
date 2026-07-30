# T-030 — `site.yml:6` references a role that doesn't exist

| Status | Priority | Size | Repo               |
| ------ | -------- | ---- | ------------------ |
| open   | P1       | S    | `~/app/ansible` — **not this one** |

## Problem

```
site.yml:6  roles/lustre   <- does not exist
```

`site.yml` names a role `lustre` that isn't on any `roles_path` entry. It is the only missing
reference across all 731 files, and it's real, not a resolver artefact — the corpus gate is
otherwise clean and 16 other unresolved role names are all the legitimate `cib-batch` case.

This is the first genuine break the project found, in a repo that has been shipping with it.
Which is also the argument for the whole project: nothing else reported it, and `site.yml` is
the top-level entry point.

## Approach

Not a code change here — a fix in `~/app/ansible`, and someone with the history has to
decide which:

- the role was renamed and `site.yml` wasn't updated -> point at the new name
- the role was deleted -> drop the entry
- it lives somewhere not on `roles_path` -> fix the path

`roles/lustre-snapshot`, `roles/lustre-nvme-binding` and a `lustre-cluster` reference all exist, so
a rename or a split is the likely story.

Tracked here rather than dropped because it is the project's first real find, and closing it is
the proof the diagnostics are worth having.

## Done when

- [ ] the intent behind `site.yml:6` is established
- [ ] `./target/release/scan ~/app/ansible` exits zero
- [ ] the outcome is recorded here — a rename, a deletion, or a path fix
