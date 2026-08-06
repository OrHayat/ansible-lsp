# T-038 — Resolve file-hitting lookups

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P2       | M    | T-120 | —          |

## Problem

Lookups that take a file path are dead-reference sites no other tool navigates, and they're
the same class as T-015's `src:` hits — just inside a Jinja call:

```yaml
- debug: msg="{{ lookup('file', 'secrets/token.txt') }}"
- template: src=... # (T-015)
- assert: { that: "{{ lookup('ini', 'db.ini section=prod key=host') }}" }
```

Affected lookups: `file`, `template`, `ini`, `csvfile`, `first_found`, `fileglob` (and the
`with_file` / `with_first_found` / `with_fileglob` loop forms). A literal path here that
resolves to nothing is a real break.

## Approach

We already pull expressions out of Jinja for `when:` and templated paths, and the glob
machinery exists (T-007). Extend the reference extractor to recognise these lookup calls with a
**literal** first argument and resolve against Ansible's documented search path (role
`files/`/`templates/`, then the task file's dir).

- `first_found` / `with_first_found`: a *list* of candidates — resolve each, error only when
  **none** exist (same first-existing shape T-016 needs).
- `fileglob`: reuse the glob resolver; "matches nothing" is the warning.
- `password`: the file is *created* on demand — a missing path is not an error. Skip.

## Done when

- [ ] `lookup('file'|'template'|'ini'|'csvfile', '<literal>')` resolves + warns on missing
- [ ] `first_found` reports missing only when every candidate is absent
- [ ] `fileglob` warns only when the pattern matches nothing
- [ ] any non-literal (templated/variable) lookup arg stays silent — unresolvable, not a bug
- [ ] the per-lookup search path is honoured (files/ vs templates/ vs task dir)

Docs: https://docs.ansible.com/ansible/latest/plugins/lookup.html ·
https://docs.ansible.com/ansible/latest/playbook_guide/playbook_pathing.html
