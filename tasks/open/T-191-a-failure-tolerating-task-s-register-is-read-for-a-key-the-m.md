# T-191 — A failure-tolerating task's register is read for a key the module does not always return

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P2       | M    | T-057      |

## Problem

## Approach

## Done when

- [ ]

## Problem

Found in `~/app/ansible`, `playbooks/tasks/select-available-node.yml`:

```yaml
- containers.podman.podman_container_info:
    name: "{{ _container_name }}"
  register: _container_check
  failed_when: false                      # failure explicitly tolerated

- set_fact:
    _node_available: "{{ _container_check.containers | length > 0 and ... }}"
```

`failed_when: false` says "carry on even if this fails". The next task then reads
`.containers` with no guard. The author was clearly thinking about absence — the inner access
carries `| default(false)` on `State.Running` — they just guarded the nested read and not the
outer one.

## Why it is provable, measured on 2.21.2

`failed_when: false` tolerates two very different outcomes, and only one of them keeps the
module's keys:

| what happened                              | register holds |
| ------------------------------------------ | -------------- |
| command exited nonzero (module itself fine) | every documented key — `cmd`, `rc`, `stdout`, `stderr`, `delta`, `start`, `end`, … |
| **module hard-failed** (bad args, missing binary, daemon down) | **`changed`, `failed`, `msg`, `failed_when_result`, `failed_when_suppressed_exception` — and nothing else** |

On the second row every documented `RETURN` key is gone, so `_container_check.containers`
raises `'dict object' has no attribute 'containers'` and the play dies — which is the outcome
`failed_when: false` was written to prevent.

Nothing static can tell the two rows apart: whether podman is installed on the target is
runtime state. That is exactly why this is diagnosable — we do not need to know *which* row
happens, only that the author opted into a path where the keys vanish and then read one
unguarded.

## Distinct from T-190

T-190 is the **skipped** case: `when:` was false, the register holds
`changed, failed, false_condition, skip_reason, skipped`. Different amplifier, different result
shape, different fixture. Both must never fire on the other's keys.

## Approach

Fire when all four hold, each visible in the same file:

1. the task tolerates failure — `failed_when: false`, `ignore_errors: true`
2. its register is read for a key that is **not** in the always-present set
3. that key's `returned:` is anything other than `always` (needs T-057, hence the dependency)
4. the read is not guarded by `default()`, `is defined`, `is not failed`, or a `when:` testing
   `.failed`

Always-present set, measured above, never to be flagged: `changed`, `failed`, `msg`,
`failed_when_result`, `failed_when_suppressed_exception`, plus T-190's skip keys.

Note `returned: always` is a promise the module can break — T-034's notes record a module in the
corpus claiming `always` for a key it omits in check mode — so condition 3 bounds the noise but
is not a guarantee.

## Done when

- [ ] the repro above fires, and the same code with `| default([])` added is silent
- [ ] silent for every key in the always-present set, one assertion each
- [ ] silent when the read is guarded by `when: not r.failed` as well as by `default()` — both
      are guards and they are separate code paths
- [ ] does not fire on T-190's skipped-task shape, and T-190 does not fire on this one — one
      test per direction, since they share a reader
- [ ] `# noqa` works, rule id matched exactly
- [ ] corpus gate: `failed_when: false` and `ignore_errors: true` are common, so count the hits
      and read every one before shipping. If it fires in the hundreds like T-190's naive form
      did, reject it rather than lowering it to a hint and shipping anyway.
