# T-063 — Port the full `include_role` / `import_role` parameter surface

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P2       | M    | T-123 | —          |

The T-017 recipe applied to role includes: read the real implementation, model every
parameter, pin behaviour with tests, and get the provable-failure diagnostics for free from
the closed arg set. Source of truth: `playbook/role_include.py` (`IncludeRole.load`) and
`playbook/role/__init__.py` (`_load_role_yaml`), read against 2.21.2.

## The arg surface (from `IncludeRole.VALID_ARGS`, a closed set)

| Param               | Default | Valid with     | Static meaning                                        |
| ------------------- | ------- | -------------- | ----------------------------------------------------- |
| `name`              | —       | both           | the role — required, **`role:` is an accepted alias** |
| `tasks_from`        | `main`  | both           | entry file in `tasks/`                                |
| `vars_from`         | `main`  | both           | entry file in `vars/` — feeds the var index           |
| `defaults_from`     | `main`  | both           | entry file in `defaults/` — feeds the var index       |
| `handlers_from`     | `main`  | both           | entry file in `handlers/` (2.8)                       |
| `apply`             | `{}`    | include only   | task keywords for the included tasks; dict or error   |
| `public`            | `no`    | include        | whether the role's defaults/vars leak to later tasks  |
| `allow_duplicates`  | `yes`   | both           | runtime dedup — nothing static                        |
| `rolespec_validate` | `yes`   | both           | runtime arg-spec check — T-041's territory            |
| `rescuable`         | `yes`   | include only   | errors catchable by an enclosing `rescue` (2.21)      |

Statically provable failures, all from `load()`'s validation and free once the set is
modelled (the include_vars precedent):

- missing `name`/`role` — "'name' is a required field"
- unknown option — "Invalid options for include_role: …"
- `apply` or `rescuable` on **import_role** — invalid there (imports inherit directly)
- a non-string value for any `*_from`

## `_load_role_yaml` — how a `*_from` value finds its file

One function decides for all four, and it is not "append .yml":

- Search dir is `<role>/<subdir>/`; the value may itself carry an extension.
- Extension order **differs by case**: explicit `X_from` tries bare name **first**
  (`['', .yml, .yaml, .json]`); the implicit `main` default tries bare **last**.
- `vars/` and `defaults/` pass `allow_dir=True`: the target may be a **directory**, all
  files under it combined (also true for the plain `main` — `defaults/main/` as a dir is
  legal and currently invisible to us). `tasks/` and `handlers/`: first match wins.
- An explicit `X_from` that matches nothing is a **hard error** ("Could not find specified
  file in role: <subdir>/<name>") — so missing-file warnings here are exact, unlike the
  default `main` which is legal to omit.
- Escape guard: the resolved file must stay inside the role (`is_subpath`) —
  `tasks_from: ../../x` fails the task.

## Measured (2.21.2), and pinned in `demo/tasks/role_include_params.yml`

Verification pass done while landing T-100; the arg table above holds, with three
corrections and one addition.

- **`name:` wins over `role:` here**, not the other way round —
  `ir.args.get('name', ir.args.get('role'))` (`role_include.py:133`). A play-level `roles:`
  entry is the mirror image: `ds.get('role', ds.get('name'))` (`definition.py:118`). Both
  spellings work in both places; only the precedence differs. Worth stating because the
  natural assumption is that one rule covers both.
- **Only `apply` and `rescuable` are include-only.** `public`, `allow_duplicates` and
  `rolespec_validate` on an `import_role` are accepted silently — the raise sites name just
  the two (`role_include.py:150-159`). Flagging a third would be a false error, now pinned
  by `only_apply_and_rescuable_are_include_only`.
- **`apply:` is the tag-propagation mechanism**, which is what makes it worth modelling
  beyond its arg validation. Live-verified: `tags: [outer]` on an `include_role` plus
  `--tags outer` runs **one** task — the include itself — and nothing inside the role. Add
  `apply: {tags: [outer]}` and all three run. This is the module doc's "tags ... are not
  automatically inherited by the include tasks, see apply", and it is a lint candidate:
  a `tags:` on a dynamic include with no `apply:` is very often not what the author meant.
- **`vars_from`/`defaults_from` confirmed costly.** `vars_from: prod` loads
  `roles/db/vars/prod.yml` and the value is usable inside the role — measured. We index
  none of it, so every key in that file is a false-positive path for `var-undefined`.

Current state is pinned from both sides:
`demo_role_include_params_resolve_or_are_documented_misses` asserts `role:`, `vars_from:`,
`defaults_from:` and `handlers_from:` produce **no reference at all** — deliberately, so
those assertions fail when this ticket lands rather than passing silently.

## Not this ticket

The module documentation's `attributes:` block (`async`, `become`, `until`, `delegation`,
`bypass_task_loop`, …) is doc metadata describing the *action's* capabilities, not keys a
playbook writes. Its consequential half — which keywords are legal on a dynamic include —
is already modelled exactly, as `VALID_INCLUDE_KEYWORDS` in `KeyContext::DynamicInclude`,
and the two agree: `become`, `until`, `connection` and `delegate_to` are absent from both.
Validating the doc block itself is T-057's.

## Approach

- A pure `role_from_file(role, subdir, name, allow_dir, fs)` mirroring `_load_role_yaml`'s
  matching (extension order per case, dir handling), unit-tested on `MemFs` like
  `include_vars::load`. The resolver and the var indexer both call it — the existing
  `TasksFrom` arm migrates onto it (verify our current extension order against the real
  one; ours predates this read).
- Extraction: read the **`role:` alias** (live gap today — `{ role: db }` yields no
  reference), emit `*_from` references with per-subdir kinds or one kind + subdir field.
- `vars_from`/`defaults_from` targets feed `vars::definitions` like role main files do.
- Provable-failure lints from the table above, same family as include_vars' unknown-param.
- `public` scoping affects definedness accuracy (T-051/T-059) — record, don't build here.

Subsumes T-042 item 3 (the `*_from` trio); T-042 narrows to its two verify items.

## Done when

- [ ] `role:` alias produces a Role reference — pinned (extraction gap, live today)
- [ ] all four `*_from` params resolve via the shared `_load_role_yaml` port, dir forms
      and extension order pinned against the real matcher
- [ ] explicit-`X_from`-missing warns (hard error upstream); absent default `main` stays
      silent
- [ ] `vars_from`/`defaults_from`/`defaults/main/`-dir keys land in the var index
- [ ] unknown option, missing name, `apply`/`rescuable` on import, non-string `*_from` —
      each a pinned provable-failure diagnostic
- [ ] corpus gate: zero new warnings on `~/app/ansible`
