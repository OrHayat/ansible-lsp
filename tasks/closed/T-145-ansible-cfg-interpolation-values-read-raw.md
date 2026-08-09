# T-145 — ansible.cfg %-interpolation: values read raw

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | S    | T-090 | —          |

## Symptom

Ansible reads `ansible.cfg` through Python's `configparser`, whose `BasicInterpolation` is
on: `%(key)s` in a value expands to another `[defaults]` value, `%%` is a literal `%`, and
` ; ` starts an inline comment (`manager.py:432` passes `inline_comment_prefixes=(';',)`).
We take values raw, so the lie runs in both directions:

- `roles_path = %(base)s/roles` works under Ansible and becomes the literal path
  `<project>/%(base)s/roles` for us — every role lookup misses, on a repo that runs fine.
- a stray unescaped `%` (URL-encoded strings, cron patterns) aborts **every** ansible
  command at startup (`InterpolationSyntaxError`) while our editor stays silent.

## Cause

The hand-rolled `[defaults]` loop in `config.rs` never modelled the configparser dialect —
adequate for `key = value` lines, wrong for the three features above.

## Fix

Interpolate each value before the key match, verified live against Python 3 `configparser`:
`%(key)s` resolves case-insensitively over the whole section (forward references included,
since configparser interpolates at read time after the full parse), `%%` unescapes, ` ; `
truncates. Where configparser aborts — unknown key, bare `%`, reference cycle — the raw
value stands and the server keeps serving, the same deliberate divergence as
[`DuplicateDictKey::parse`]: going dark over one config line is worse.

Out of scope, recorded for honesty: the `[DEFAULT]` fallback section (interpolation can
reference it in configparser; we don't read it), `key: value` colon syntax, and multi-line
continuation values. Nothing observed in real configs needs them yet.

## Done when

- [x] `%(key)s` expands from `[defaults]`, forward and case-insensitive references included
- [x] `%%` reads as a literal `%`
- [x] inline ` ; ` comments are stripped from values
- [x] where configparser aborts (unknown key, bare `%`, cycle) the raw value stands
- [x] each behaviour is unit-tested, with the configparser run that verified it cited
