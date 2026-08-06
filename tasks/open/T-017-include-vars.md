# T-017 — `include_vars`

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P2       | M    | T-120 | —          |

## Problem

30 references, all dead. Task-level, and it has more forms than the other include kinds:

```yaml
- include_vars: x.yml                       # bare string
- include_vars: { file: x.yml }             # file:
- include_vars: { dir: vars/, extensions: [yml] }   # a DIRECTORY, not a file
```

Many are templated, so most of the value comes through the glob path.

## Approach (agreed 2026-08-02)

`ReferenceKind::IncludeVars`, plus a separate `IncludeVarsDir` kind for `dir:` — the two need
different existence tests (`is_file()` vs `is_dir()`), so one kind with a flag does not work.

~~Search order: role `vars/` -> role dir -> playbook dir.~~ **Wrong — see Findings.** The two
forms use entirely different lookups: `dir:` is one computed path (`_set_root_dir`), `file:`
goes through `_find_needle('vars', …)`. Neither is the list above.

`dir:` resolution mirrors `_set_root_dir` — one computed path per case, never a search list:

| Case                                       | Path                    | Diagnostic                             |
| ------------------------------------------ | ----------------------- | -------------------------------------- |
| in role, starts `vars/`, role path exists  | `<role>/<input>`        | none                                   |
| in role, starts `vars/`, role path missing | `<cwd>/<input>`         | warn: fragile — works or not depending |
|                                            |                         | on the launch directory (see Findings) |
| in role, otherwise                         | `<role>/vars/<input>`   | missing dir -> plain warning           |
| not in role                                | `<task dir>/<input>`    | missing dir -> plain warning           |

`Resolved` = the **directory** exists — an empty directory is legal and resolves. Never test
any particular file inside it. Navigation `targets` are the files the directory loads
(recursive, default extensions `yaml`/`yml`/`json`), via the multi-candidate picker idiom;
`status` stays the directory's verdict.

The cwd-fallback warning is a soft warning, not an error — cwd-relative can legitimately work
if runs always start from the same directory. It states the provable fact (role path absent)
plus the consequence, never a claim that the path won't resolve.

Also worth knowing: `include_vars` is a task, so it can carry `when:`/`loop:`. That's already
captured by `TaskContext` and needs nothing new.

## Done when

- [x] bare, `file:` and `dir:` forms all handled distinctly (incl. free-form `dir=` k=v)
- [x] `dir:` checks directory existence only, and navigates to **the files it loads**
- [x] navigation targets filtered by the module's real semantics (extensions & friends)
- [ ] templated forms glob without warning — no warning today, but templated `dir:` is
      skipped with no navigation candidates; the deduped glob-for-navigation is still to do
- [x] the `dir:` base path follows `_set_root_dir` — one computed path, not a search list
- [ ] the in-role `vars/`-prefixed missing case warns about the cwd fallback — resolution
      pinned by test (Missing + the role-relative candidate); the *message* still says plain
      missing-dir, the fallback wording needs the diagnostic layer
- [x] corpus gate: zero new warnings (byte-identical scan), non-zero `dir:` count from the
      demo and tests — the corpus itself has no `dir:` forms (Findings, 3)

## Findings — 2026-08-02, read against ansible-core 2.21.2

A first attempt at this is in `git stash` ("T-016/T-017 wip"). It passed its tests and the
corpus gate, and it was still wrong in three ways. Read these before restarting.

**1. The search order above is invented, and so was the code's.** Line 21 says "role `vars/`
-> role dir -> playbook dir". Ansible's `_set_root_dir`
(`plugins/action/include_vars.py:157`) does no searching at all — it computes **one** path:

| Context | `dir:` resolves to |
| ------- | ------------------ |
| in a role, value starts `vars/` | `<role>/<value>` if it exists, else the value unchanged (cwd-relative) |
| in a role, otherwise | `<role>/vars/<value>` — unconditionally, no existence check |
| not in a role | `<directory of the task's own file>/<value>` |

Note the last row: **not** the playbook dir and **not** the project root. Searching extra
places means reporting `Resolved` for a directory Ansible would never load — a false green,
which is worse here than a false warning.

The "value unchanged" fallback in row 1 is **cwd-relative**: the unresolved value goes
straight to `walk()` (line 187), and `ansible-playbook` never chdirs (the only `chdir` in the
CLI is `ansible-pull`'s). So the same playbook loads different vars — or fails — depending on
the launch directory, silently, with no `-vvv` trace. Ansible's only environment-dependent
path in this module, and the motivation for the fragile-fallback warning in the Approach.

**2. Navigation must target the files, not the directory.** Resolving to the directory
produces *"The file is not displayed in the text editor because it is a directory"* on
Ctrl+click. Point `targets` at the files the directory loads; that is already the
multi-candidate/picker idiom used for templated paths. Keep `status` as the *directory's*
verdict so an empty directory still resolves — an empty one is legal.

**3. ~~The corpus gate passed vacuously.~~ Disproven 2026-08-02: the corpus has no `dir:`
forms at all.** `grep -rn include_vars -A3 | grep dir:` over `~/app/ansible` finds
nothing — the extractor never dropped anything; there was nothing to extract. The non-zero
reference count for the gate comes from the demo (`include_vars_demo.yml`: two resolving
`dir:` tasks, one deliberately missing) and the unit tests instead.

## Options — decided 2026-08-02 (superseded same day: full support landed)

~~Only the default extension filter is honoured.~~ The user directed a full port instead:
`include_vars.rs` implements the whole loading semantics — `extensions`, `depth`,
`files_matching`, `ignore_files` (real regex semantics, `regex` dep added), `name`, the
free-form k=v line — as a pure function over an `Fs` trait, pinned by in-memory tests, five
live `ansible-playbook` runs, and Ansible's own splitter test table. Extraction and the
resolver call it; nothing *warns* based on the filter options, they only pick targets.

Real semantics, for whoever does: `extensions` defaults to `['yaml','yml','json']` and is a
*validation* — an unlisted extension **fails the task** unless `ignore_unknown_extensions:
true`. `depth: 0` means unlimited, and the walk is recursive by default. `ignore_files`
entries are end-anchored **regexes**, not file names (an Ansible docs bug; live-proved on
2.21.2: `ignore_files: [bastion.yaml]` also skips `edge-bastion.yaml` — and the unescaped
`.` matches any character). Draft report: `upstream/ansible-include_vars.md`, together with
the dead `vars/main.yml` guard.

The full arg surface, from the action plugin's `_set_args` + valid-arg sets (the plugin, not
the docs, does the validation):

| Param                       | Type      | Default                   | Valid with |
| --------------------------- | --------- | ------------------------- | ---------- |
| `file`                      | path      | —                         | file form  |
| free-form (`_raw_params`)   | path      | — (fallback if no `file`) | file form  |
| `dir`                       | path      | —                         | dir form   |
| `depth`                     | int       | `0` = unlimited           | dir form   |
| `files_matching`            | str/regex | none -> no filter         | dir form   |
| `ignore_files`              | list      | none -> `[]`              | dir form   |
| `extensions`                | list      | `['yaml','yml','json']`   | dir form   |
| `ignore_unknown_extensions` | bool      | `false`                   | dir form   |
| `name`                      | str       | none -> top-level vars    | both       |
| `hash_behaviour`            | str       | none -> global config     | both       |

The three sets are closed (`VALID_FILE_ARGUMENTS`, `VALID_DIR_ARGUMENTS`, `VALID_ALL`), so an
unknown param — or a dir-form param on a `file:` call, e.g. `extensions` with `file:` — is a
statically provable task failure. That diagnostic is in the split-out list, not this ticket.

## Split out — do not do these here

- **`_find_needle` port.** The `file:` form uses `_find_needle('vars', …)`
  (`ActionBase` + `DataLoader.path_dwim_relative_stack`), not `_set_root_dir`. Shared
  machinery: also wanted by T-015 and T-038. Our `include_vars_file_bases` differs from it in
  four ways, including a `source_root != dirname` guard we lack.
- **`name:` namespacing.** `include_vars: { file: x.yml, name: db }` puts every key under
  `db`. `vars.rs::read_var_file` indexes them top-level, so it defines variables that do not
  exist and misses the one that does. Live bug, unrelated to this ticket, T-053 code.
- **Provable-failure diagnostics.** The arg list is closed, so an unknown parameter and mixing
  `dir:` with `file:` are both statically provable task failures. Cheap, no filesystem needed.
