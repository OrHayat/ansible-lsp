# T-177 — add_host variables are not indexed, so a later play reads them as undefined

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| partly done | bug | P1    | M    | —          |

## Symptom

`add_host` defines host variables. We index none of them, so any later play that uses one is
reported as using an undefined variable. Measured — this playbook runs fine and the scan says:

```yaml
- hosts: localhost
  gather_facts: false
  tasks:
    - ansible.builtin.add_host:
        name: "{{ item }}"
        groups: k8s_pods
        pod_namespace: my-namespace
      loop: [a, b]

- hosts: k8s_pods
  gather_facts: false
  tasks:
    - debug:
        msg: "ns={{ pod_namespace }}"
```

```
UNDEFINED VARIABLES (1):
  site2.yml:14  pod_namespace
```

P1 because it is the failure this project exists to avoid: the variable is genuinely defined,
the play works, and the squiggle is wrong. A false `var-undefined` is worse than none.

It surfaced on the documented replacement for a removed inventory plugin —
`kubernetes.core.k8s` was removed in `kubernetes.core` 6.0.0, and its deprecation message
prescribes `k8s_info` + `add_host`. So this is the pattern people are being *told* to migrate
to, not an exotic one.

It hides today only by accident. The same playbook written with the connection variables
Ansible's own docs use (`ansible_connection`, `ansible_kubectl_namespace`) reports nothing —
not because we understand `add_host`, but because every `ansible_*` name is unconditionally
exempt. Rename the variable and the false positive appears.

## Cause

Nothing reads `add_host` as a definition site. The only `add_host` handling in `vars.rs` is a
negative: a test asserting that a *templated* key is not a definition, because measured, the
host var is then really named `{{ k }}` and the intended name is never set (that half is
T-170). The literal keys beside it — the ones that do define variables — were never picked up.

## Fix

Treat an `add_host` task's literal argument keys as definitions, minus the module's own
parameters. Templated keys stay excluded, which is already measured and already asserted —
do not regress it while adding this.

**The parameter list is not the alias list, and this ticket had it wrong.** The line above
used to read "minus `name`/`hostname`, `groups`/`group`". Measured on 2.21.2 by diffing each
spelling's created host against a `name:`-only baseline:

| written       | creates the host/group | also leaves a variable |
| ------------- | ---------------------- | ---------------------- |
| `name:`       | yes                    | no                     |
| `hostname:`   | yes                    | no                     |
| `groups:`     | yes                    | no                     |
| `groupname:`  | yes                    | no                     |
| `host:`       | yes                    | **yes — `host`**       |
| `group:`      | yes                    | **yes — `group`**      |

The set to exclude is upstream's own `special_args`, `('name', 'hostname', 'groupname',
'groups')` (`action/add_host.py:85`) — `groupname`, which the old wording omitted, and *not*
`host`/`group`, which it named. `host:` and `group:` are read as aliases when the plugin
picks the name (`:52`) and the group list (`:68`) and are then left in `args`, so each also
lands in `host_vars` under its own name. Taking the four from the module's documented
aliases would have dropped two real definitions and indexed one name that is not one.

Implemented as `ADD_HOST_PARAMS` in `vars.rs`, with the measurement at the site.

The leak is upstream's bug, dossiered in
[`upstream/ansible-add-host-alias-leak.md`](../../upstream/ansible-add-host-alias-leak.md) —
the docs declare six interchangeable spellings and the code excludes four, with nothing
keeping them in sync because the module file is documentation-only. We index `host`/`group`
on purpose because that is what ansible does; if upstream takes the fix, this constant grows
the two names.

## Measured (2.21.2)

Two of the three questions below are now answered. One playbook: play 1 `add_host`s `newhost`
into group `created`, which an ini inventory already populates with `preexisting`; play 2
targets `created` and prints every collision. Each source also contributes a **unique** name,
so a collision the loser was never present for cannot be misread as a win.

| collision, on `newhost`          | winner            | so add_host is |
| -------------------------------- | ----------------- | -------------- |
| add_host vs `group_vars/created` | **add_host**      | above group_vars |
| add_host vs `host_vars/newhost`  | **host_vars**     | below host_vars  |
| add_host vs play `vars:`         | **play vars**     | below 15         |
| add_host vs `set_fact`           | **set_fact**      | below 21        |

Bracketed on both sides, that is **level 8, "inventory file or script host vars"** — the rung
between playbook `group_vars/*` (7) and inventory `host_vars/*` (9). Controls all alive in the
same run: `only_group`, `only_addhost`, `only_play`, `only_hostvars` each resolved.

**Scope, settled by the same run.** `preexisting` is in the same group, in the same play, and
read `addhost=UNDEF` — add_host variables reach only the hosts add_host created, never the
group they were added to. That is the control that makes the scope claim a measurement rather
than an assumption.

**Ordering is not an axis, and `hostvars` sees it.** Both measured on 2.21.2, in one run
with the controls alive:

| probe                                                  | result                    |
| ------------------------------------------------------ | ------------------------- |
| read on the *calling* host, **before** the task         | `UNDEF`                   |
| read on the calling host, **after** the task            | `UNDEF`                   |
| `hostvars['newhost'].addhost_var`                       | `FROM_ADDHOST`            |
| positive control `hostvars['localhost'].fact_var`       | `FROM_SET_FACT`           |
| negative control `hostvars['localhost'].play_var`       | `UNDEF`                   |
| the created host, in the next play                      | `FROM_ADDHOST`            |

The two controls are what make the hostvars row a measurement: the same expression printed a
value for a source known visible and `UNDEF` for one known invisible, so it could have
reported either way. So `visible_to_hostvars()` is **true**.

Ordering needed no caveat, and for a better reason than the generous rule: the calling host
never gets the variable *at any point*, so a same-file earlier use is not a case that exists.
`add_host` therefore stays out of `Located::ordered_before` — putting it in, by analogy with
`set_fact`, would have invented a false positive rather than prevented one.

Consumers to assert individually: `undefined_uses`, hover, and go-to-definition, which must
land on the `add_host` key.

## What is knowable from the file, and what is not

The question that keeps coming back is whether the fix needs the set of hosts `add_host`
created. It does not, and the two tables are why.

Readable without running anything — all four literals:

| fact                                        | from            |
| ------------------------------------------- | --------------- |
| there is an `add_host` task                 | the task itself |
| it defines a variable named `pod_namespace` | a literal key   |
| its value is `my-namespace`                 | a literal value |
| the created hosts join group `k8s_pods`     | `groups:`       |

Not readable, and nothing recovers it:

| fact                          | why                                                  |
| ----------------------------- | ---------------------------------------------------- |
| *which* hosts were created    | `name: "{{ item }}"` over `loop: "{{ pods.resources }}"` |
| whether the task ran at all   | a `when:`, or an empty loop                          |

`var-undefined` asks only "does this name have a reachable definition", so the first table is
the whole of what it needs. Do **not** grow `calls_add_host` into a host-set collector to
serve this ticket: its one consumer, `unknown_host_diagnostics` (`main.rs:693`), uses it as a
file-wide veto, and a set that misses a runtime-named host converts a missed report into a
false ERROR — the trade `main.rs:678-680` deliberately refused.

**The `groups:` literal is knowable and still must not narrow the claim.** It is tempting to
scope the definition to plays targeting `k8s_pods`. Measured, that is false: `preexisting`,
already in that group from the inventory and in the same play, read `addhost=UNDEF`. Only the
created hosts get the variable, so "defined for plays on `k8s_pods`, undefined elsewhere"
trades one wrong answer for another. Where the literal genuinely pays is a rule this ticket
does not write: a play whose `hosts:` names a group no inventory declares matches nothing,
and Ansible says only `skipping: no hosts matched` at exit 0. Any such rule must count
`add_host`'s `groups:` values as declared groups, or it will warn on working code.

**And it must read all four spellings of that value.** `groups:` is not a scalar — upstream
takes a list or a string and splits the string on commas, stripping each
(`action/add_host.py:70-76`). Measured on 2.21.2, one host per form, reading `groups` back
and checking membership so a form that silently created nothing could not pass:

| written              | declares                         |
| -------------------- | -------------------------------- |
| `groups: a`          | `a`                              |
| `groups: [a, b]`     | `a`, `b`                         |
| `groups: "a,b"`      | `a`, `b`                         |
| `groups: "a, b"`     | `a`, `b` — the space is stripped |

A reader that handles only the scalar form misses half the declarations and warns on working
code, which is the failure the rule exists to avoid. `groupname:` and `group:` reach the same
code path, so they need the same treatment — and note `group:` is *also* an ordinary variable
(see the Fix table), so it is the one value that has to be read twice, for two purposes.

## What comes free once the keys are indexed

`VarDef` (`vars.rs:152`) already carries `source` and a `span` pointing at the **value**, so
one change lights up four existing features rather than one:

- hover shows `my-namespace` plus the provenance line (T-052, T-066) — done, asserted
- go-to-definition lands on the defining line (box 5 below) — done, asserted
- precedence is answerable against a colliding `group_vars`/`host_vars` — level 8, measured
- `templates/{{ pod_namespace }}/x.j2` resolves, because the value is a known literal (T-056)

## Adjacent — verified, and now T-179

Reproduced: an `add_host` in an imported task file does not silence `unknown-host` in the
playbook, which reports a read that ansible runs clean (`ok=2 failed=0`). Both controls held
— inline `add_host` stayed quiet, a genuinely absent host still fired. Filed as **T-179**;
nothing about it is this ticket's to fix.

The original note, kept for the reasoning:

`calls_add_host` is passed `a.nodes` (`main.rs:315`), which is **one file's** parse. Read
plainly, an `add_host` inside a role or an included task file therefore does not silence
`unknown-host` in the playbook that reads `hostvars['thathost']` — a false ERROR on working
code. Not run yet; rule 1 says that makes it a hypothesis. If it reproduces it is a P1 bug of
its own, not part of this ticket.

## Done when

- [x] the reproduction above reports zero undefined variables, pinned by test —
      `a_variable_defined_by_add_host_is_not_undefined_in_a_later_play`, with the control
      that the same playbook minus the one defining line still reports `pod_namespace`.
      **Mapping args only.** The free-form spelling `add_host: name=h ff_var=V` also
      defines a variable (measured: it read back in the next play) and is **not** indexed,
      so the same false positive survives there. Pinned as a deliberate miss by
      `the_free_form_add_host_spelling_is_a_known_miss`; splitting `_raw_params` is
      `parse_kv`/shlex semantics and belongs to T-046, which should flip that test.
- [x] a templated `add_host` key still defines nothing — the T-170 control, re-asserted here
      as `a_templated_add_host_key_defines_nothing`, with a literal key beside it as the
      control. Re-measured in this shape: the created host's keys really are
      `['literal_beside_it', '{{ dyn }}']` and `dyn` reads `UNDEF`.
- [x] ~~`name`/`hostname`/`groups`/`group`~~ `name`/`hostname`/`groupname`/`groups` are not
      themselves indexed as variables — the box was **wrong as written**, see the Fix
      section. `host:` and `group:` are aliases that do work *and* leave a variable behind,
      so they are indexed on purpose. Asserted both ways in
      `add_host_argument_keys_are_definitions_but_its_own_parameters_are_not`.
- [x] precedence and scope measured, each with a probe that could report either way — level
      8, controls `only_group`/`only_addhost`/`only_play`/`only_hostvars` all alive in the
      same run; scope settled by `preexisting` reading `addhost=UNDEF` from inside the group.
      Ordering is **not** covered by this box and is still open, above.
- [x] hover and go-to-definition answer from the `add_host` key, asserted per consumer —
      `an_add_host_variable_hovers_and_jumps_to_the_key_that_defined_it`, one assertion
      each, plus the consumed parameter `name` as the control that neither may answer for.
      The definition's span points at the **value**, with play vars rather than with
      `set_fact`: both are the same line so the jump is unchanged, and only that side lets
      hover read the value out. Demo labels pinned separately by
      `the_add_host_demo_flags_the_consumed_parameter_and_nothing_it_defines`, which asserts
      the exact set (2) rather than only that the good rows are quiet — without the fix it
      is 6.
- [ ] corpus gate: the count can only fall; every disappeared line confirmed `add_host`-defined
      — **blocked, not skipped.** `~/app/ansible` is not on this machine (T-016 records it as
      permanently off it, and it is absent from both the Windows side and the WSL install
      that has ansible-core 2.21.2). Everything else here is measured or pinned; this box
      needs the corpus to be mounted and is the only reason the ticket is not closed.
