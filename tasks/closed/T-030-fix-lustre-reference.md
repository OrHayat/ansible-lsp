# T-030 — `site.yml:6` references a role that doesn't exist

| Status       | Priority | Size | Repo                               |
| ------------ | -------- | ---- | ---------------------------------- |
| **rejected** | P1       | S    | `~/app/ansible` — **not this one** |

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

## Outcome — rejected 2026-08-18

Not worth fixing: the file is unused. Re-measured on the current tree:

- `site.yml` was last modified in the **initial commit** (`Ansible initial version`, 2025-09-07)
  and never touched since
- nothing in the tree includes or imports it — its only 7 mentions are prose in role
  `README.md` files (`ansible-playbook site.yml --tags ...`)

The break is real and still the only MISSING FILE in 759 files, but it sits in dead scaffolding.
Owner's call, taken deliberately rather than by neglect.

Worth keeping in view: those READMEs still advertise `site.yml` as the entry point, so anyone
following them runs a playbook that fails immediately. That is a documentation problem in
`~/app/ansible`, not a tool problem, and not this board's business.

What the tool could have said here is **not** "missing role" — it is "this playbook is
unreferenced". That is T-021, and this case exposes a limit worth carrying there: a top-level
playbook is *supposed* to be unreferenced, since it is an entry point. What identified this one
as dead was git history and prose-only mentions, neither of which the reverse index sees.
