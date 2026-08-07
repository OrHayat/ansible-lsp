# T-102 — Duplicate YAML mapping key

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P1       | S    | T-099 | —          |

## Problem

Two `when:` on one task, or two `name:`, or the same variable twice in a `vars:` block. The
last one wins and the earlier is dead. Ansible warns:

```
Found duplicate mapping key 'when'. Using last defined value only.
```

`_internal/_yaml/_constructor.py:63-82`, under `DUPLICATE_YAML_DICT_KEY` — default `warn`,
also `error` or `ignore` (`config/base.yml:1361-1375`). A warning in a play run scrolls past;
in an editor it is a squiggle on the exact line.

Two wrinkles worth pinning:

- Severity should follow the user's `ansible.cfg`, since `error` is a legal setting and some
  repos use it.
- Ansible tries `json.loads` on **every** file before YAML (`parsing/utils/yaml.py:38-49`),
  so a `.yml` file containing JSON is parsed as JSON and duplicate keys there are silently
  last-wins with no warning at all. Our parser should match that asymmetry rather than
  improve on it.

## Approach

libyaml reports every key event with a mark; collecting duplicates per mapping is bookkeeping
in `parse_libyaml.rs`, not a second parse. This is the cheapest rule on the board — the
information is already flowing through the parser and is currently dropped.

What landed, in order:

**Last-wins is now correct.** `Node::get` took the *first* occurrence while Ansible takes the
last, so on a duplicate key we resolved and explained the value Ansible discarded — a
duplicated `include_tasks:` linked the dead path, a duplicated `when:` was analysed as the
dead condition. `parse.rs:56` and `ast.rs:302` now search from the end
(`duplicate_key_resolves_to_the_last_value_like_ansible`, `duplicate_import_playbook_takes_the_last`).
That matters for the diagnostic too: **both** entries are still in the tree, so the rule
underlines the later one and the value the rest of the tool reads is that same one.

**The fixture exists.** `demo/duplicate_keys.yml` covers all three placements; the JSON half
is `demo/plays/duplicate_keys_json.yml`. Both live-verified against ansible-core 2.21.2 —
four warnings, each anchored at the later occurrence, and silence for the JSON file.

**The setting is read.** `AnsibleConfig::duplicate_dict_key` is a `DuplicateDictKey` enum
(`Warn` default, `Error`, `Ignore`). Mind the two spellings: the ini key is
`duplicate_dict_key` under `[defaults]`, the env var is `ANSIBLE_DUPLICATE_YAML_DICT_KEY`.
Precedence is env > ini > default, live-verified in both directions (cfg `error` + env
`ignore` runs silently; cfg `ignore` + env `error` refuses the file), and the env var applies
with no `ansible.cfg` present — `tests/duplicate_dict_key_env.rs`.

Values are case-sensitive and exactly `error`/`warn`/`ignore`. Anything else — including the
`False` that the option's own description still recommends — aborts *every* ansible command
with `Invalid value ... Valid values are: error, warn, ignore`. We deliberately diverge and
fall back to the layer below instead: a language server that goes dark over one config line
is worse than one that analyses with the shipped default.

T-098 caps how right this can be: we only ever read `project_root/ansible.cfg`, so we may
read the setting correctly from the wrong file.

**The rule fires.** `parse_libyaml::duplicate_keys` walks the event stream `build` already
produces — a second walk, not a second parse — and mirrors `build`'s key/value pairing so it
cannot report a duplicate the tree doesn't have. Scoped per mapping, so sibling tasks sharing
a key don't collide, and it returns the loser's span as well as the winner's because the
message names both. `main.rs::duplicate_key_diagnostics` maps `Error`/`Warn` onto the LSP
severities and returns nothing at all for `Ignore`; `# noqa: duplicate-key` suppresses a line.

Checked against the fixture rather than reasoned about: the rule reports the same four keys
on the same four lines Ansible warns about (23 `hosts`, 29 `http_port`, 37 `when`, 42 `name`).

**The JSON path reports anyway, unlike Ansible** (decided; the original line here said to
match the asymmetry). Ansible tries `json.loads` before YAML (`parsing/utils/yaml.py:41`,
comment: *"Fixes issues with extra vars json strings"*), so a JSON-content file never reaches
the constructor where the duplicate check lives, and no severity — not even `error` — fires
there. That exemption is collateral from a helper shared with `-e` extra-vars, not a decision
about playbooks, and the data loss is identical.

So JSON behaves **exactly** like YAML — same severity from `duplicate_dict_key`, and `ignore`
still silences it — with one sentence appended saying Ansible does not report this one and
why. No separate severity tier: the duplicate is equally real, and a reader who wants the
distinction gets it from the message rather than from a colour.

`parse::Loader` (`Yaml`/`Json`) records which parser Ansible would use, decided by an actual
`serde_json` parse because no heuristic reproduces it — `[{"a":1,"a":2}]` and the same text
with a trailing comma differ by one byte, and Ansible warns about only the second (verified).
It lives on `Document`, **not** `FileContext`, because `FileContext` is memoized per
*directory* (`cache.rs:272`) while a loader is per file — `demo/plays/` already holds one of
each. Gated on the first non-space byte so the common case is a single comparison.

Known gap: CPython's `json` accepts bare `NaN`/`Infinity`, `serde_json` rejects them, so such
a file is JSON to Ansible and YAML to us. Left alone deliberately — JSON-content `.yml` files
are already a curiosity, and `scan` can measure it if that ever stops being true.

## Done when

- [x] duplicate keys in a mapping produce one diagnostic per later occurrence
- [x] severity follows `DUPLICATE_YAML_DICT_KEY`, and `ignore` suppresses entirely
- [x] the JSON path still reports, saying why Ansible is silent there
- [x] a fixture covers duplicates in `vars:`, in a task, and at play level
