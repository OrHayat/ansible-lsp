# T-149 — Validate meta/argument_specs.yml itself, not just call sites

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P3       | M    | T-123 | —          |

## Problem

T-041 checks the vars a *call site* passes against a role's spec. The spec **file** has no
coverage of its own: a typo'd option attribute (`requird:`), a bad `type:` value, or a
malformed entry-point mapping is invisible until the role actually runs with
`validate_argspec` on — and one broken spec then fails every caller of the role at once.

Note the split with T-107 machinery: `argument_specs` as a *key* in `meta/main.yml` is
legal (`metadata.py:41`, covered by T-147); the *contents* follow the argument-spec
schema (`type required default choices elements options description version_added
aliases …`), which is module-argspec territory, not `FieldAttribute`s.

## Approach

Audit first, T-107 discipline: read the path that consumes a role spec —
`validate_argument_spec` down into `ArgumentSpecValidator`
(`module_utils/common/arg_spec.py`) — and record what an invalid spec *does* at runtime
(hard error, warning, or silently ignored attribute) with cites, before choosing
severities. Then check entry-point mappings and per-option attributes against that
verified vocabulary. Same trap as T-041: absence of a spec means silence, and the rule
must never fire on roles that don't ship one.

## Done when

- [ ] the spec-consuming path is read whole; per-attribute behaviour recorded here
      with cites
- [ ] unknown option attributes and invalid `type:` values are flagged at the severity
      the audit justifies, on the offending key's span
- [ ] valid specs — including the demo's — stay silent, as do roles with no spec
- [ ] `# noqa`-suppressible
