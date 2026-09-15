# T-233 — Capture each ansible-core release's builtin modules and enforced argument specs into a bundled table

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | L    | T-123 | —          |

## Problem

Two gaps, one data source.

**No Ansible detected.** Every module outside the workspace hovers "Skipped — not in this
workspace (a builtin, or installed outside it)", `ansible.builtin.copy` and a made-up
`nosuch.coll.thing` alike, and definition goes nowhere. Measured over LSP with `PATH` and
`HOME` stripped (T-232 follow-up). The server knows nothing about builtins unless it can read
an install.

**No parameter truth even with one.** Nothing checks `copy: {pathh: x}`. T-057 plans to read
input options from `DOCUMENTATION`, and says that is what `AnsibleModule(argument_spec=...)` is
built from. Measured below: it is not, and a warning built on the docs gives wrong answers.

## Measured, 2026-09-16

Latest patch of every minor, 2.14.18 through 2.21.4, each pulled into a throwaway `uv` env.
`scratchpad/t233_probe.sh` reruns all of it.

**Module names move.** 69 of 74 builtin modules ship in all eight cores. The other five:

| Module              | Ships in      |
| ------------------- | ------------- |
| `yum`               | 2.14 – 2.16   |
| `_include`          | 2.14 – 2.15   |
| `dnf5`              | 2.15 – 2.21   |
| `deb822_repository` | 2.15 – 2.21   |
| `mount_facts`       | 2.18 – 2.21   |

A list taken from one core calls `mount_facts` a builtin for a 2.17 user and never knew `yum`.
Only a per-core table can say "shipped in 2.14–2.16".

**Documented options: additions are recorded, removals are not.** 856 option paths across the
eight cores' merged `ansible-doc` output, 729 in all of them.

- 28 options were added to modules that already existed. Per-option `version_added` matches
  the first core that ships the option for 27; the 28th, `git.gpg_allowlist` ("2.9", first
  shipped 2.17), is a rename that kept the old option's date.
- 7 left the docs. No option in any of the eight cores carries a structured `deprecated` key
  (only modules do: `apt_key`, `apt_repository` in 2.21), so the docs cannot say an option is
  going. What each one actually was:

| Option                     | What happened                                                              | Where it is data |
| -------------------------- | -------------------------------------------------------------------------- | ---------------- |
| `dnf.install_repoquery`    | removed; `removed_in_version='2.20'` in `yumdnf_argument_spec`             | argument spec    |
| `git.gpg_whitelist`        | became an alias; `deprecated_aliases=[{name, version: '2.21'}]`             | argument spec    |
| `yum_repository.keepcache` | a `module.deprecate(..., version='2.20')` call when the key is set         | code only        |
| `script.decrypt`           | documented via the `decrypt` doc fragment, absent from the action plugin's spec | nowhere — never accepted |
| `lineinfile.others`        | a doc placeholder ("all `file` arguments work here"), never a parameter    | not a parameter  |
| `replace.others`           | same name and removal shape as `lineinfile.others`; not inspected           | —                |
| `dnf5.install_repoquery`   | same doc text as `dnf`; its spec not inspected                              | —                |

**The docs are not what is enforced.** `script` with `decrypt: false` on 2.20, where the docs
still list `decrypt`: `Unsupported parameters for (...script) module: decrypt. Supported
parameters include: _raw_params, chdir, cmd, creates, executable, removes`. Control: `bogus:
false` fails with the same message. 2.21 rejects `decrypt` the same way. So a parameter check
built on 2.20's own docs would call a rejected parameter valid.

**The enforced spec can be captured.** `scratchpad/t233_capture_argspec.py` runs a module with
`AnsibleModule.__init__` patched to record `argument_spec` and abort, and an action plugin with
`ActionBase.validate_argument_spec` patched the same way. Nothing past validation runs.

| Core   | Target                  | Captured                                                                          |
| ------ | ----------------------- | --------------------------------------------------------------------------------- |
| 2.19.13 | `dnf.install_repoquery` | `{"removed_in_version": "2.20", "type": "bool"}`                                  |
| 2.17.14 | `git.gpg_allowlist`     | `aliases: [gpg_whitelist]`, `deprecated_aliases: [{name: gpg_whitelist, version: "2.21"}]` |
| 2.20.9  | `script` (action)       | `_raw_params, chdir, cmd, creates, executable, removes` — the exact list the real rejection printed |

## Approach

A generator, run by a developer, never by the server:

1. For each core minor, `uv` installs its latest patch into a throwaway env.
2. List `modules/` and `plugins/action/` filenames and read `release.py` — imports nothing.
3. Capture every builtin's enforced spec as above: option names, `aliases`, `type`, `choices`,
   `required`, `removed_in_version` / `removed_at_date`, `deprecated_aliases`.
4. Take `version_added` and descriptions from `ansible-doc -j` for hover text only.
5. Merge into one table: module → cores it ships in → action twin → per option, the cores that
   accept it and its deprecation data. Record the exact cores measured.
6. Check it in as JSON under `crates/ansible-core`, embedded with `include_str!`. The first
   bundled Ansible data the server reads at runtime; the test corpora are the only precedent.

**Consumers, in order of risk:**

- **Hover when no install is detected.** Every claim is about measured cores, never the user's:
  "`ansible.builtin.yum` — shipped in core 2.14–2.16, removed in 2.17 (bundled table; Ansible
  not detected here)". A name the table does not have gets no claim. A detected install always
  wins, and a workspace `library/` module still shadows a bare name.
- **Option hover and completion**, same labelling: "`armor` — added in 2.20".
- **Unknown or removed parameter hints.** The only consumer that can mislead, so it gets the
  strictest rule: it fires only when the version is known (a detected install inside the
  table's range) and the captured spec for that exact minor rejects the key. No version, no
  hint. Coordinate with T-057 — this is the input half it planned to take from `DOCUMENTATION`.

## Traps

- **`add_file_common_args=True`** adds `mode`, `owner`, `group`… outside the spec dict. The
  capture above records 12 parameters for `copy` without them; it must merge
  `FILE_COMMON_ARGUMENTS` when the flag is set.
- **Action plugins that consume arguments** before calling the module (`copy`, `template`):
  the accepted set is the plugin's handling plus the module's spec. `copy`'s action plugin
  has no `validate_argument_spec` call in 2.20 or 2.21, so there is nothing to intercept there.
- **Action-only builtins** (`set_fact`, `include_tasks`, `meta`, …) never build an
  `AnsibleModule`; they need the action path or an explicit "not captured" row. A silent gap
  would make every key look unknown.
- **`module.deprecate()` calls** (`keepcache`) are invisible to the capture. Record them as not
  captured rather than as absent.
- **Import side effects**: capture runs module code up to validation. Throwaway envs only.
- **Python pins**: 2.14 and 2.15's `ansible-doc -j` aborts under Python 3.14 with "missing
  documentation" on `add_host`, which has docs, and works under 3.11; 2.21 requires Python ≥ 3.12.
- **A new core ships.** The table's newest core is a floor, not a ceiling: a name or key absent
  from it may be new. Regeneration belongs next to `board upstream --live`'s release check.

## Done when

- [ ] a generator script produces the table from real cores, recording which cores it measured
- [ ] every captured spec is checked against Ansible's own "Supported parameters include" list
      for that module and core — a mismatch fails the generator
- [ ] `add_file_common_args`, action-plugin-consumed keys, and action-only builtins are each
      handled or marked not captured, with a test naming one example of each
- [ ] with no install detected, `ansible.builtin.yum` hovers its measured core range and
      `nosuch.coll.thing` gets no builtin claim — pinned tests, one per surface
- [ ] a detected install always answers instead of the table, pinned by a test with both present
- [ ] an unknown-parameter hint fires only with a detected core inside the table's range, and
      never on `git.gpg_whitelist` under a core whose captured spec lists it as a deprecated
      alias (2.17 and 2.20 measured; gone from 2.21's spec)
- [x] T-057's input-schema plan points here, and its claim that `DOCUMENTATION.options` is what
      `argument_spec` is built from is corrected
