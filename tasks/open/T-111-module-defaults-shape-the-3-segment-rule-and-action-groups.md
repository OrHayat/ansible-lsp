# T-111 — module_defaults: shape, the 3-segment rule, and action groups

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-106 | T-064      |

## Problem

`module_defaults` has its own resolution rules, and they are not the ones anywhere else in
the file uses. `_load_module_defaults` (`playbook/base.py:243-297`):

- must be a dict, or a list of dicts — fatal otherwise
- keys must be **static**; a templated key is rejected
- a key with fewer than three dot-separated segments is prefixed `ansible.legacy.`
  (`base.py:262-265`)
- **the `collections:` keyword is deliberately ignored here**, unlike every other short name
  in the play

Then the severities fork:

| Case | Behaviour | Cite |
| ---- | --------- | ---- |
| top-level key that resolves to nothing | fatal `Could not resolve action %s in module_defaults` | `base.py:410-411` |
| unknown `group/<name>` | fatal | `base.py:332,345` |
| a **member** of a group that does not resolve | `display.vvvvv` only | `base.py:371-373,412` |
| `extend_group` target that does not resolve | silent | `base.py:387` |
| any of it, when `self.play is None` | group resolution skipped entirely | `base.py:270-271` |

So the top-level case is loud and the group-member case is invisible at any verbosity a human
uses.

## Approach

The dict/list shape and the 3-segment rule are pure syntax and can ship first. Group
resolution needs `action_groups` from collection `meta/runtime.yml`, which is T-064's parse —
hence the dependency.

Worth stating in the code: `action_groups` affect **only** `module_defaults`, never module
name resolution (`base.py:266-273,314-397`). T-042 already concluded that; this is the one
place they do matter.

## Done when

- [ ] a non-dict/list `module_defaults` is an ERROR
- [ ] a templated key is an ERROR
- [ ] a short key is resolved as `ansible.legacy.*` and ignores `collections:`
- [ ] an unresolvable top-level key is an ERROR; an unresolvable group member is a HINT
- [ ] `group/<name>` is checked against `action_groups` once T-064 lands
