# T-153 — Playbook .meta files: the playbook-level argument_specs

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P3       | S    | T-123 | —          |

## Problem

Found during the T-107 whole-file read of `play.py`, not in any docs we'd been using: a
play with `validate_argspec:` makes Ansible look for a sibling **`<playbook>.meta{ext}`**
file (`site.yml` → `site.meta.yml`, candidates built at `play.py:452-461`) holding
`argument_specs.<name>.options` — a playbook-level argument spec, resolved at
post-validate time (`play.py:396-450`). The named spec missing, or the meta file absent
while `validate_argspec: true`, is a hard `AnsibleParserError` at load. We treat
`*.meta.yml` as nothing at all: no file-kind, no go-to from the `validate_argspec:`
value, no diagnostic when the spec it names doesn't exist.

## Approach

Small and precise, anchored on the keyword: when a play carries `validate_argspec:`,
resolve the candidate paths exactly as `_metadata_candidate_paths` does (extension list
from `YAML_FILENAME_EXTENSIONS`), then (1) ERROR when no candidate exists or the named
spec/`options` is missing — replicating the messages at `play.py:420-448`; (2) make the
value a navigable reference to the spec entry. The spec *contents* follow the same
schema as T-149 — share that audit, don't duplicate it.

## Done when

- [ ] `validate_argspec:` with no `.meta` sibling, or naming a spec that isn't there,
      gets an ERROR with Ansible's message
- [ ] the value is a resolvable reference (go-to lands on the spec entry)
- [ ] a play without `validate_argspec:` never looks for the file; `.meta.yml` files are
      otherwise untouched
- [ ] `# noqa`-suppressible
