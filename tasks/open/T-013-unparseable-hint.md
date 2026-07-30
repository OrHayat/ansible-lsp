# T-013 — Hint on unparseable files

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P1       | S    | —          |

## Problem

An unparseable file yields no references and no diagnostics — correct behaviour (see the
board's *Settled* table: PyYAML accepts a file YAML 1.2 rejects, so unparseable must never
mean "broken"). But it's **indistinguishable from a file with no references**, and that
silence is a trap.

Evidence: writing `demo/tasks/main.yml` broke its own YAML three times in a row — an unquoted
`: ` inside a `name:` value, as in `name: Block form with an explicit file: parameter`. Each
time the server went completely quiet, and each time the reaction was "the feature is broken,"
not "the file doesn't parse." That's three occurrences in one small hand-written file; a real
repo will hit it too.

Unquoted `: ` in a task name is the single most common way to do this by accident.

## Approach

Publish one `DiagnosticSeverity::HINT` per unparseable file, at the position saphyr reports,
code `unparseable`. Wording has to carry the nuance — this is not a claim the file is invalid
Ansible:

> Not valid YAML 1.2, so references in this file are not analysed. Ansible's parser may
> still accept it.

Suppressible with `# noqa: unparseable`, which matters for
`roles/daos-nvme-binding/tasks/_run.yml` — a known-good file that legitimately fails strict
parsing and shouldn't nag forever.

Count it in `bin/scan.rs`'s output too; today "1 unparseable" is a bare number with no way to
find out which file.

## Done when

- [ ] breaking the YAML in `demo/tasks/main.yml` produces a visible hint, not silence
- [ ] `# noqa: unparseable` silences it
- [ ] `scan` names the unparseable files instead of just counting them
- [ ] severity is HINT, never WARNING — the file may be perfectly valid to Ansible
