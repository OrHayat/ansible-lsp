# T-016 — `vars_files`

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | M    | —          |

## Problem

75 references, all dead. Play-level, so it resolves like `import_playbook` rather than like a
task include.

Two wrinkles seen in the real repo:

- several entries use `../` paths, so normalisation has to happen before the existence check
- an entry can be a **list**, meaning "use the first of these that exists" — a legitimate
  first-match-wins construct, so a missing earlier entry is not an error

## Approach

`ReferenceKind::VarsFiles`. Search order: playbook dir -> `<playbook_dir>/vars/` -> role
`vars/`.

The list form needs care: for `vars_files: [[a.yml, b.yml]]` the play succeeds if *any* of the
inner entries resolves. Warn only when **none** of them do, and anchor the diagnostic on the
whole sequence rather than on individual entries — otherwise correct code gets a warning on
the entries that were meant to be absent.

Templated entries behave as everywhere else: glob for navigation, never warn.

## Done when

- [ ] all 75 navigate, or are explained (templated / outside workspace)
- [ ] `../` paths normalise before the check
- [ ] a nested list warns only when every entry is missing
- [ ] corpus gate: zero new warnings
