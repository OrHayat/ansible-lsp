# T-016 — `vars_files`

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| done   | P2       | M    | —          |

## Problem

75 references, all dead. Play-level, so it resolves like `import_playbook` rather than like a
task include.

Two wrinkles seen in the real repo:

- several entries use `../` paths, so normalisation has to happen before the existence check
- an entry can be a **list**, meaning "use the first of these that exists" — a legitimate
  first-match-wins construct, so a missing earlier entry is not an error

## Approach

`ReferenceKind::VarsFiles`. Search order — **corrected against live ansible-core 2.21.2**
(`vars/manager.py:320-383` via `path_dwim_relative_stack`, `dataloader.py:345-390`), which
disproved this ticket's original "playbook dir → playbook vars/ → role vars/" claim:
`<play dir>/vars/<entry>` (skipped when the entry's first component is literally `vars`),
then `<play dir>/<entry>`. Nothing else — no role `vars/`, no project root, no extension
guessing; absolute/`~` entries are one candidate. And a missing file is **silently
ignored** at runtime since the first-found loop's error was lost upstream (its "we raise
an error" comment is stale) — the warning is still right, because the variables silently
never load, but its wording must not claim the play fails.

The list form needs care: for `vars_files: [[a.yml, b.yml]]` the play succeeds if *any* of the
inner entries resolves. Warn only when **none** of them do, and anchor the diagnostic on the
whole sequence rather than on individual entries — otherwise correct code gets a warning on
the entries that were meant to be absent.

Templated entries behave as everywhere else: glob for navigation, never warn.

## Done when

The corpus (`~/app/ansible`, 75 refs) is private and permanently off this machine, so the
first and last boxes are rewritten onto the in-repo acceptance surface (T-077 direction):

- [x] every `vars_files` case in `demo/` navigates or is explained (templated /
      absent-by-design), pinned by LSP-level tests
- [x] `../` paths normalise before the check
- [x] a nested list warns only when every entry is missing
- [x] `scan demo` reports exactly the labeled BAD cases and nothing else; full suite green

## Landed

`VarsFilesEntry` keeps the first-match grouping in the AST (`ast.rs`), each alternative is
its own `VarsFiles` reference and a multi-alternative entry adds one group reference that
alone can be Missing — members downgrade to `Skipped(GroupAlternative)`, so nav/decoration
work per file while the warning anchors on the whole list and `scan` counts a group once.
`resolve::vars_files_candidates` is the single ported search-order function, shared with
the var index (`vars.rs`), which also gained the first-found fix: only the winning
alternative is indexed (both used to be), and the `vars/`-subdir copy now wins over the
play-dir copy, matching what Ansible loads. Demo expanded into the acceptance surface:
plain/bare-string/`../`/no-extension/templated/`{{ playbook_dir }}`/group cases, plus
`demo/plays/vars_files_parent_demo.yml`.
