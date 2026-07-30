# T-008 — Teal decoration for resolvable references

| Status | Priority | Size | Commit  |
| ------ | -------- | ---- | ------- |
| done   | P1       | S    | 0135475 |

## Problem

*"I don't know that it's clickable until I try, because you told me."* Navigation that works
but is invisible is navigation nobody uses. Ansible YAML has no syntax cue for "this string
is a file path."

## Outcome

A custom `ansible/references` request returns every reference with its span and resolution
status; the client paints resolvable ones teal with a dotted underline. Unresolvable stays
plain text, so the colour itself is the signal — you can see a typo before clicking it.

**Why a custom request and not `documentLink`.** documentLink underlines natively, but a
link's target *overrides* the definition provider, so a templated reference with several
candidates would silently jump to one of them. Colouring and navigation had to be separated:
links only for the single-target case, decoration for everything.

`demo/tasks/main.yml` and `demo/playbook.yml` exercise every kind, labelled GOOD/BAD, so the
behaviour is visible without hunting through 731 real files.
