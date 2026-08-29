# T-015 — `template:`/`copy:` `src:` + the local-vs-remote table

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P2       | L    | T-120 | —          |

## Problem

385 `src:` occurrences and every one is currently dead. It's the largest single block of
unnavigable references left, and templates are exactly the files you want to jump to.

**But `src:` does not always mean a local file**, and that's what makes this L rather than M.
Counted by owning module in `~/app/ansible`:

| Module        | Count | `src:` means                                    |
| ------------- | ----- | ----------------------------------------------- |
| `copy`        | 126   | local — role `files/`                           |
| `template`    | 123   | local — role `templates/`                       |
| `synchronize` | 7     | local                                           |
| `slurp`       | 27    | **remote** — a path on the managed host         |
| `file`        | 14    | **remote** — symlink source                     |
| `fetch`       | 13    | **remote**                                      |
| `unarchive`   | 15    | **depends on `remote_src:`**                    |

Roughly 256 local, 54 unconditionally remote, 15 conditional. Checking a remote path for
local existence produces warnings on correct code — `src: "/sys/devices/system/node/node{{ … }}/nr_hugepages"`
is a read on the managed host and there is nothing to find locally. That's the second
false-positive source this project has identified, after the role-`tasks/` resolution order.

(Counts are from a 6-line lookback grep, so treat them as approximate; the AST gives the exact
owning module.)

## Approach

`modules.rs`, an explicit table keyed by normalised module name (FQCN and short form both):

```rust
enum ArgPath { Local, Remote, DependsOn(&'static str) }
```

- `Local` -> resolve and diagnose
- `Remote` -> `Skipped { reason: RemotePath }`, never diagnosed
- `DependsOn("remote_src")` -> read the sibling key off the same mapping node; **absent
  defaults to local**, matching Ansible's default of `remote_src: false`
- **anything not in the table is `unknown` and never diagnosed.** The table will be
  incomplete; incompleteness must cost navigation, never produce a false warning

Search order per the resolution table: `template` -> role `templates/` -> `<playbook_dir>/templates/`
-> role dir -> playbook dir; `copy` -> the same with `files/`.

Also: `dest:` is *always* remote and must never be treated as a reference. Worth an explicit
test so nobody adds it later by pattern-matching on "path-shaped argument".

## Partly done: the `template:` row, taken by T-040

T-040 needed the `.j2` -> rendering-task link and nothing else could supply it, so
`ReferenceKind::TemplateSrc` landed there. What exists now:

- `template:` `src:` is a reference, both the mapping and the `src=x dest=y` k=v spelling
- it **resolves**, in this ticket's stated order: role `templates/`, `<playbook_dir>/templates/`,
  role dir, playbook dir, then the task file's own directory
- it **navigates** — go-to-definition on a `src:` value
- `resolve::render_sites` inverts it: every `template:` task that renders a given `.j2`

What is deliberately **not** done, and is still all of this ticket's risk:

- **no diagnostic.** `TemplateSrc` is filtered out of `diagnostics_of` explicitly. The
  missing-file verdict on `src:` is this ticket's and belongs behind the corpus gate below —
  385 `src:` values in one real tree is exactly the shape that produces a warning wave.
- **no table.** Only `template:` is recognised. Every other module's `src:` produces no
  reference at all, which is this ticket's own rule ("anything not in the table is unknown and
  never diagnosed") reached by having no table rather than by having one.
- `copy:`, the remote modules, `unarchive`'s `remote_src:`, and the `dest:` guard are untouched.

So the first Done-when row is half true and stays unticked: `template:` navigates, `copy:` does
not, and neither warns.

## Re-run the magic-variable survey when this lands

`scan` prints `VARIABLES USED IN TEMPLATED PATHS`. Today it shows 11 distinct variables,
of which the only knowable ones — `role_path` (4) and `playbook_dir` (4) — are already
expanded rather than globbed. But that survey only covers reference kinds the extractor
supports, and `src:` is the largest unsupported block. Template and file paths are exactly
where `{{ role_path }}/files/x.conf` is idiomatic, so expect new entries and check whether
any are magic variables that should be expanded instead of treated as unknown.

## Done when

- [ ] `template:`/`copy:` `src:` navigate to role `templates/`/`files/`
- [ ] `slurp`/`fetch`/`file` `src:` return `RemotePath` — tested, not just absent
- [ ] `unarchive` respects `remote_src:`, defaulting to local when the key is missing
- [ ] a module absent from the table yields no diagnostic
- [ ] `dest:` is never a reference
- [ ] corpus gate: zero new warnings across all 731 files
- [ ] the templated-variable survey re-run, and any new magic variables expanded
