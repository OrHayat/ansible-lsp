# T-172 — Diagnose a hostvars read of a play-scoped variable

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | task | P2       | S    | T-062      |

## Problem

```yaml
vars:
  play_scoped: 8080
tasks:
  - debug: msg="{{ hostvars['web01'].play_scoped }}"   # undefined, always
```

`hostvars` is assembled with no play and no task, so play `vars:`, `vars_files:`, role
defaults/vars/params and block/task vars are invisible through it — measured across all
thirteen sources in T-104. When *every* definition of a name is one of those, the read is
dead on every host, and Ansible's own error names an internal type
(`'HostVarsVars' has no attribute 'play_scoped'`) without ever saying the variable exists.

Built in T-104 and **removed the same day**. It is filed again rather than left in that
ticket's history because the work is real and wanted — just not yet correct.

## Why it needs T-062

The rule reduces to "every definition I can *see* is play-scoped". Inventory is the source
`hostvars` is most answered by, and the one we cannot read, so that reduction is false:

```yaml
vars: {both_places: FROM_PLAY_VARS}      # play
node1 both_places=FROM_INVENTORY         # inventory.ini
```

Measured — the read returns `FROM_INVENTORY` and works. We would have called it "always
undefined" on working code. The same blindness in the other direction cost 37 corpus false
positives when the rule also fired on names it found nowhere.

With inventory indexed, both cases resolve: `both_places` is visible and stays silent,
`play_scoped` is genuinely only play-scoped and warns.

## Approach

The retracted implementation is in `T-104` and in git; all of it still applies. What was
removed is one clause in `undefined_uses_in` (`&& !u.through_hostvars`) and one branch in
`variable_coverage_diagnostics`. What survives and should not be rebuilt: the extraction
(`condition::hostvars_uses`), `VarSource::visible_to_hostvars`, `VarUse::through_hostvars`,
and `Located::reaches`.

The message was also built and is worth restoring as written — verdict first, the fix at the
end, and the definition carried as a `related_information` link rather than the word "here":

```
always undefined: `play_scoped` is a play var, and `hostvars` cannot see those — it is
built without the play. Move the value to `host_vars/`, `group_vars/`, or a `set_fact`.
  ↳ hostvars.yml:20  "`play_scoped` is defined here, as a play var — out of reach from `hostvars`"
```

## Done when

- [x] a read whose every definition is play-scoped warns, with the link
- [x] a name also defined in the inventory stays silent — the case that retracted it,
      asserted with a real inventory fixture
- [x] a name defined nowhere stays silent
- [x] corpus gate: `var-undefined` count unchanged at the then-current baseline
- [x] `demo/hostvars.yml`'s two BAD rows lose their "NOT FLAGGED" notes

## Landed

`undefined_uses_in` judges a `hostvars` read when every definition of the name is invisible to
`hostvars`, at least one exists, and the inventory and `add_host` sets are both knowable. The
read's own `hostvars` root is checked for `| default` / `is defined`. Re-measured on 2.21.3 in
one run: the play var through `hostvars` fails with `'HostVarsVars' has no attribute
'play_scoped'`, a direct read of it prints 8080, the same name also on an inventory host line
returns `FROM_INVENTORY`, and `| default('D')` prints `D`. The message links the definition
through `related_information`. A vars plugin supplying the name is T-228's gap, which this rule
shares with every `var-undefined`.

Corpus gate: `var-undefined` is 315 with and without the rule; control on `demo/` is 14 → 16,
the two BAD rows.
