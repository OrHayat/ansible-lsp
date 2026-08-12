# T-104 — hostvars cannot see play, role or task vars

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P1       | M    | T-099 | —          |

## Problem

```yaml
vars:
  api_port: 8080
tasks:
  - debug: msg="{{ hostvars['web1'].api_port }}"   # always undefined
```

`HostVars.__getitem__` calls `get_vars(host=host)` with **no play and no task**
(`vars/hostvars.py:44-57`). So `hostvars[x]` sees inventory, group and host vars, facts and
extra vars — and cannot see play `vars:`, `vars_files`, role defaults/vars/params, or
block/task vars, for *any* host including the current one.

This is structural, not timing: no run order makes it work. A `hostvars[...]` reference to a
name whose only definitions are play-scoped is **provably** always undefined, which is a
stronger claim than T-051 can usually make and needs none of its exemptions.

Second, smaller fact from the same file: `'localhost' in hostvars` is True because the
membership test auto-creates the implicit localhost (`:69-71`), while `list(hostvars)` omits
it (`:73-77`). A `for h in hostvars` loop and an `in` check disagree.

## Measured (2.21.2)

Source-derived when filed; run since, against `demo/inventory.ini`. One play, `hosts: all`,
`vars: {play_scoped: 8080}`:

| read                                             | result         |
| ------------------------------------------------ | -------------- |
| `{{ play_scoped }}`                              | `8080`         |
| `{{ hostvars['web01'].app_port }}` (host_vars)   | `8888`         |
| `{{ hostvars['web01'].play_scoped }}`            | **fails**      |
| `{{ hostvars[inventory_hostname].play_scoped }}` | **fails**      |

The last row is the one to lead the message with: the same variable, on the host already
running the task, in the same task — direct read fine, hostvars read fatal. The group_vars
row is the control that proves hostvars was working rather than the fixture being wrong.

Ansible's own error is

```
object of type 'HostVarsVars' has no attribute 'play_scoped'
```

which names an internal type, not the variable, and never says the definition exists and is
out of reach. Our message has to say the part Ansible's does not — which source we found it
in, and why that source cannot be seen from here.

`demo/hostvars.yml` carries all four rows; every label there was checked by running it.

## Approach

The variable index already records a source per definition (`vars.rs`, `VarSource`). The
verdict is a filter over it: for a use inside `hostvars[...]`, consider only sources at or
below host/group level. No new source and no new file reads.

### Correction: there is no use to filter yet

Filed as "no new walk". Measured against our own extractor, that is wrong — the walk
produces **nothing** for these expressions:

| expression                          | extracted today  |
| ----------------------------------- | ---------------- |
| `hostvars['web01'].app_port`        | *(nothing)*      |
| `hostvars['web01']['app_port']`     | *(nothing)*      |
| `hostvars[target_host].app_port`    | `target_host`    |
| `cmd_result.stdout`                 | `cmd_result`     |

`hostvars` is dropped as an injected name and `app_port` is never reached, because the
extractor takes the root of an expression and an attribute/subscript is not one. So the
first job is extracting the *subscripted* name out of a `hostvars[...]` chain, and only
then can any source filter run.

That extraction is also the whole of the GOOD half, which is currently just as broken:
`hostvars['web01'].app_port` resolves to `host_vars/web01.yml` and is perfectly reachable
at runtime (measured, `8888`), and we offer no hover and no jump on it. One change serves
both halves — navigation where the source is visible, a diagnostic where it is not.

Size was filed `S` on the filter-only reading; `M` is the honest number.

### The visibility split is not a precedence level

Filed as "sources at or below host/group level". Measured instead — all thirteen, cross-host
on 2.21.2 (web02 reading web01):

| visible                          | invisible                           |
| -------------------------------- | ----------------------------------- |
| `group_vars/`, `host_vars/`      | play `vars:`, `vars_files:`         |
| `include_vars`                   | role defaults, role vars            |
| `set_fact`, `register`           | role params, `roles:` entry `vars:` |
|                                  | block `vars:`, task `vars:`         |

The line is whether the source wrote into the *host's* storage or hung off the play/role/task
object. `include_vars` is visible and `vars_files` is not — both load a YAML file of
variables, and only the first calls `register_host_variables`
(`action/include_vars.py:149`, the same door `set_fact` uses at `set_fact.py:44`). A
precedence reading puts `include_vars` on the wrong side and warns on working code.

### Only the provable half can be reported

The first cut warned whenever no visible definition was found. Against the corpus that was
**37 new warnings, and all 37 were wrong** — `infiniband_ip`, `hostname`, `private_ip`,
`vips`: per-host variables written in `inventory*.yml`, which we do not parse (T-062).

`hostvars` is answered mostly by inventory, so "not found" here means "not looked", and the
base case is not evidence at all. The rule now fires only where the name **is** defined and
every definition is out of reach — which no inventory can change, and which is the claim the
Problem section actually makes. Corpus after the fix: 677 undefined-variable reports, exactly
the number before the ticket.

## Done when

- [x] `hostvars[h].x` is extracted as a use of `x` at all — the attribute and the
      `['x']` subscript spelling alike, since neither produces one today
- [~] `hostvars[h].x` where every definition of `x` is play-scoped is diagnosed —
      **retracted**, see below; needs T-062 to be sound. The message work (verdict-first,
      `related_information` link) was built and removed with it.
- [x] the same name defined in `group_vars/` does not warn, and *does* hover and jump —
      the GOOD half is broken today too, and one extraction fixes both
- [x] a templated host key still resolves the *variable* half
- [x] `demo/hostvars.yml`'s rows are pinned by a test, and its `NOT YET FLAGGED` notes are
      removed once they are
- [x] a name no indexed source defines stays **silent** — inventory is invisible, so the
      base case is not evidence, and a corpus gate holds the count flat

### Retracted: the diagnostic is not sound without T-062

Shipped, then removed the same day. The rule was "every definition I can see is
play-scoped, therefore this read is always undefined". Measured counter-example:

```yaml
vars: {both_places: FROM_PLAY_VARS}          # play
node1 both_places=FROM_INVENTORY             # inventory.ini
```
```
direct           = FROM_PLAY_VARS
through hostvars = FROM_INVENTORY            # works
```

`hostvars` reads the inventory value, so the line is correct and we would have called it
"always undefined". Inventory is exactly the source `hostvars` is *most* answered by and
exactly the one we cannot read, which makes the visible definitions unrepresentative in
both directions — the 37-false-positive gate below was the same fault caught one case
earlier, and gating only that case was half a fix.

The corpus made the cost/benefit plain: the rule fired **zero** times across 753 files. A
rule that never fires on real code and has a false-positive path is not worth its risk.

**What survives, and why it is sound:** the extraction, `visible_to_hostvars`, and the
navigation filter. Hover and go-to-definition decline to offer a play var for a `hostvars`
read — true whatever the inventory holds, since the play var is definitely not what that
read returns. Only the claim that the read is *broken* needed inventory.

Refiled as T-172, blocked on T-062.

### The first message was too long and pointed nowhere

Shipped as 322 characters opening with "`x` is defined here (play var), but ..." — "here"
naming no location, the verdict buried behind the mechanism, and the full list of reachable
sources inlined as reference material. Reported from the editor as unreadable, and fairly.

Now 176 characters, opening with `always undefined:`, ending with the fix, and carrying the
definition as a `related_information` link the editor renders as a jump. The source list
lives in the demo and in this ticket, which is where reference material belongs. A test
holds the ceiling at 200 characters so it cannot creep back.

## Landed

`condition::hostvars_uses` (both spellings, refusing the dynamic second subscript),
`VarSource::visible_to_hostvars` (the measured table), `VarUse::through_hostvars`, and one
reachability rule — `Located::reaches` / `in_effect_for` — that all **four** consumers now
share: the undefined check, the uncovered-`when` check, the resolved-reference decoration,
and hover/go-to-definition. Splitting it per caller is what T-100 already cost us once.

Not done, deliberately: the `'localhost' in hostvars` / `list(hostvars)` disagreement noted
in the Problem section is still source-derived and unmeasured. It is a separate claim about
membership rather than lookup, and belongs in its own ticket once someone runs it.
