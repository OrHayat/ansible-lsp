# Upstream proposal — redesign the variable interface (ansible/proposals#127)

**Status:** filed · an open design proposal by a core maintainer, not ours, nothing built

Not a bug, and nothing to file. This records the closest upstream discussion to a
**variables spec**: a way to declare which names exist, so a tool can tell an unseen input
from a typo. Read 2026-10-21 with `gh issue view 127 -R ansible/proposals`; nothing was
posted.

**Upstream state:**

- [ansible/proposals#127](https://github.com/ansible/proposals/issues/127) — "redesign
  variable interface", opened 2018-07-05 by bcoca. Still open. Proposal status "New", type
  "core design", target "future release". Three comments, the last from 2019.

## What it proposes

Its motivation is the problem this LSP keeps meeting: "Unclear scopes makes it hard to know if
a variable is available or not."

- Named scopes: Global, Play, Host, Defaults and Builtins, each addressable as a dict
  (`Play[varname]`), with a bare name taking the one with most precedence.
- A short precedence order: Builtin > Host > Play > Global > Defaults, last defined wins
  within each.
- Role vars private by default, optionally namespaced by role.
- `set_fact` replaced by a `set` action with explicit conflict resolution and type.
- `vars_files` and `include_vars` deprecated in favour of lookups that must be assigned to a
  name, so they cannot create top-level variables.

It does **not** propose declaring variables or a schema for a vars file.

## Comments

- A user asks for roles to be able to return variables instead of leaking them.
- alikins links [PR #28477](https://github.com/ansible/ansible/pull/28477) ("Varman show
  precedence", closed unmerged): tracking which scope each variable came from, for debugging.
- [ansible/ansible#6189](https://github.com/ansible/ansible/issues/6189) is linked as related:
  "Role vars and role defaults are not available in hostvars", 2014, closed.

## What does exist instead

Declarations exist only as input signatures, not for vars files in general:

| File                               | Declares        | Since              | Here             |
| ---------------------------------- | --------------- | ------------------ | ---------------- |
| `roles/<r>/meta/argument_specs.yml` | a role's inputs | ansible-core 2.11  | T-041, T-149     |
| `<playbook>.meta.yml`              | a play's inputs | 2.20, tech preview | T-153 done, T-240 |

A name declared there is a known input even when its value comes from somewhere we cannot
read, which is the case T-037 needs.

Counted 2026-10-21: 11 of 45 public repos use role argument specs (135 of 911 roles), and
no repo has a playbook `.meta.yml`.
