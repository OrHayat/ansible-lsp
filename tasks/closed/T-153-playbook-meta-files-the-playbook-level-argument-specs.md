# T-153 — Playbook .meta files: the playbook-level argument_specs

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P3       | S    | T-123 | —          |

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

## Resolution

Measured 16 shapes on 2.21.2 (`scratchpad/t153/probe.sh`, one dir per case). Every fault also
fails `--syntax-check`, so it is a load error and an ERROR on every core. Three findings the
source read did not settle:

- A quoted `"true"` is a spec *name* (`No argument spec named 'true'`), and a plain `yes` is the
  boolean. The parse tree has no scalar style (T-162), so quoting is read off the source text.
- With both `site.meta.yml` and `site.meta.yaml` present, the first candidate is read even when
  only the second holds the spec.
- `false`, `""` and a templated value never look for the file.

`validate_argspec::sites` gives one answer per play: `Found { meta, entry }`, with the spec's key
span, or `Fault(message)`, with Ansible's message (`Error processing keyword
'validate_argspec': …`). Candidate paths are listed by file name instead of absolute path. The
`validate-argspec` diagnostic is anchored on the value and
suppressible by rule id. Go-to-definition on the value reads the same sites and lands on the
spec's key in the meta file. A meta file that does not parse, and a templated play name, give no
site: both fail some other way at run time.

`YAML_FILENAME_EXTENSIONS` is configurable upstream and not read here; the default list is used,
as in `cache.rs`. The spec's *contents* (T-149) and treating it as the play's declared inputs
(T-240) are still those tickets'.

Tests: `each_validate_argspec_shape_gets_ansibles_answer` (every probed row),
`a_found_spec_points_at_its_key`,
`validate_argspec_naming_a_missing_spec_is_an_error_on_the_value` (severity, anchor, `# noqa`,
and the meta file itself staying clean), and
`definition_on_validate_argspec_lands_on_the_spec_entry`. All four were seen red with `sites`
forced empty.

## Done when

- [x] `validate_argspec:` with no `.meta` sibling, or naming a spec that isn't there,
      gets an ERROR with Ansible's message
- [x] the value is a resolvable reference (go-to lands on the spec entry)
- [x] a play without `validate_argspec:` never looks for the file; `.meta.yml` files are
      otherwise untouched
- [x] `# noqa`-suppressible
