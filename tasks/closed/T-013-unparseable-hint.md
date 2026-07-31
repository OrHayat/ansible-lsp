# T-013 — Hint on unparseable files

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| done   | P1       | S    | —          |

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
`roles/lustre-nvme-binding/tasks/_run.yml` — a known-good file that legitimately fails strict
parsing and shouldn't nag forever.

Count it in `bin/scan.rs`'s output too; today "1 unparseable" is a bare number with no way to
find out which file.

## Done when

- [x] breaking the YAML produces a visible diagnostic, not silence (demo: `tasks/unparseable.yml`)
- [x] `# noqa: unparseable` silences it (demo: `tasks/unparseable_silenced.yml`)
- [x] `scan` names the unparseable files instead of just counting them
- [x] ~~severity is HINT, never WARNING~~ → **ERROR**, see note

## Closing note — severity flipped to ERROR

Originally shipped as a grey HINT, on the reasoning that a strict-YAML-1.2 failure might still
be valid to Ansible. **T-036 removed that possibility**: the parser now matches Ansible
(libyaml), so a file we can't parse is one Ansible can't load either — a play that includes it
fails. So the diagnostic is now a red **ERROR** with an honest message. The `# noqa` escape
hatch stays, for files you know don't parse standalone (a Jinja-templated `.yml`, a partial
include). See T-037 (vault): a vaulted file isn't YAML and must be exempted so this error
doesn't false-positive.
