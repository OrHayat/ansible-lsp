# T-177 — add_host variables are not indexed, so a later play reads them as undefined

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P1       | M    | —          |

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
parameters (`name`/`hostname`, `groups`/`group`). Templated keys stay excluded, which is
already measured and already asserted — do not regress it while adding this.

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

Still open, and the only thing left to probe before the code (rule 1): **ordering.** The
definition exists only after the task runs. `var-undefined`'s documented rule is that any
reachable definition exempts, even a later one, so this should not need ordering — confirm
rather than assume, since routing through `in_effect_at` once before imported a `set_fact`
rule that broke exactly that (see CLAUDE.md rule 3).

`visible_to_hostvars()` also needs its own answer. Precedence does not settle it: the split
is whether the source wrote into the *host's* storage, and `add_host` plausibly did.

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

## What comes free once the keys are indexed

`VarDef` (`vars.rs:152`) already carries `source` and a `span` pointing at the **value**, so
one change lights up four existing features rather than one:

- hover shows `my-namespace` plus the provenance line (T-052, T-066)
- go-to-definition lands on the `add_host` key (box 5 below)
- precedence is answerable against a colliding `group_vars`/`host_vars` — level 8, measured
- `templates/{{ pod_namespace }}/x.j2` resolves, because the value is a known literal (T-056)

## Adjacent, unverified

`calls_add_host` is passed `a.nodes` (`main.rs:315`), which is **one file's** parse. Read
plainly, an `add_host` inside a role or an included task file therefore does not silence
`unknown-host` in the playbook that reads `hostvars['thathost']` — a false ERROR on working
code. Not run yet; rule 1 says that makes it a hypothesis. If it reproduces it is a P1 bug of
its own, not part of this ticket.

## Done when

- [ ] the reproduction above reports zero undefined variables, pinned by test
- [ ] a templated `add_host` key still defines nothing — the T-170 control, re-asserted here
- [ ] `name`/`hostname`/`groups`/`group` are not themselves indexed as variables
- [x] precedence and scope measured, each with a probe that could report either way — level
      8, controls `only_group`/`only_addhost`/`only_play`/`only_hostvars` all alive in the
      same run; scope settled by `preexisting` reading `addhost=UNDEF` from inside the group.
      Ordering is **not** covered by this box and is still open, above.
- [ ] hover and go-to-definition answer from the `add_host` key, asserted per consumer
- [ ] corpus gate: the count can only fall; every disappeared line confirmed `add_host`-defined
