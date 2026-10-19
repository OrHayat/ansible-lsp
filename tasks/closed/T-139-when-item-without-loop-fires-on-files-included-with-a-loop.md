# T-139 — when-item-without-loop fires on files included with a loop

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | bug  | P1       | M    | T-020      |

## Symptom

We warn `` `item` is only defined inside a loop … the condition can never evaluate `` on
conditions that run fine. Found by the T-077 corpus (`condition::corpus`), which classifies
1211 real `when:` sites from four official collections: **every** problem it reported was this
one, three times, and all three were wrong.

| Upstream file (`ansible-collections/…`) | Condition |
| --- | --- |
| `community.general` `cmd_runner/tasks/test_cmd_echo.yml:12` | `item.copy_to is defined` |
| `community.general` `alternatives/tasks/test.yml:49` | `ansible_facts.os_family != 'RedHat' or with_alternatives or item != 1` |
| `community.general` `alternatives/tasks/test.yml` | `ansible_facts.os_family == 'RedHat' and not with_alternatives and item == 1` |

Open any of those in an editor and we mark a working line broken. That is the P1 definition —
the tool lies — and it is the worst kind of false positive, because the advice ("this can never
evaluate") is confidently wrong and invites deleting a correct guard.

## Cause

`item` comes from the `include_tasks` **site**, not from the task carrying the `when:`:

```yaml
# cmd_runner/tasks/main.yml
- ansible.builtin.include_tasks:
    file: test_cmd_echo.yml
  loop: "{{ cmd_echo_tests }}"      # <- item is bound here
```

```yaml
# test_cmd_echo.yml — every task in this file sees `item`
- ansible.builtin.copy: {src: /bin/echo, dest: "{{ item.copy_to }}/echo"}
  when: item.copy_to is defined     # <- we flag this
```

`condition::problems(cond, has_loop)` takes `has_loop` from the task's own mapping only —
`references.rs` sets `repeated` by looking for `loop:`/`with_*` as a sibling key, and
`main.rs:546` passes it straight through. Nothing consults the including file. The same applies
to `with_sequence: start=1 end=2` on an `include_tasks` inside a `block:`
(`alternatives/tasks/tests.yml:12-13`), which is how the other two arise.

The rule itself is sound: with `has_loop = true` all three come out clean. Only its input is
missing.

## Fix

`item` must be treated as possibly-bound in any file reachable from a looped `include_tasks`.
Needs the invocation chain (T-020) for the general case — the same dependency T-137 and T-068
have, and for the same reason: a task file's meaning depends on who included it.

A file can be included from several places, so the honest rule is "if **any** include site
loops, don't flag" — flagging requires proving no caller loops, which an unindexed workspace
cannot do. Until the chain exists, the safe interim is to suppress the rule in task files
entirely and keep it for playbooks, where the task and its loop are always in one file: a
missed warning costs nothing, a false one costs trust.

## Resolution

Took the interim, and it is the whole answer: the reverse index (T-020) cannot prove no caller
loops, because a role's task file has callers outside any workspace. `expressions::sites`
marks `item` as bound for every site in a task file, so the server, `scan` and the demo test
all read it from one place. Measured on 2.21.2: a looped `include_tasks` and a looped
`include_role` each bind `item` in the included file, even under an inner task's
`loop_control: loop_var:`; an unlooped include dies with `'item' is undefined`.

The rule stays a warning in playbooks: a play `vars: {item: 2}` makes `when: item == 2` run
(measured), and the rule does not read variables. The BAD rows moved from
`demo/tasks/conditions.yml` to the playbook `demo/item_conditions.yml`, pinned by
`the_item_conditions_demo_matches_its_annotations_exactly` and guarded by
`every_other_demo_file_is_free_of_item_without_loop`.

## Done when

- [x] the three corpus cases stop being reported
- [x] `condition::corpus::item_from_an_including_loop_is_not_flagged` passes with its
      `#[ignore]` deleted. It was `..._is_flagged_today_and_should_not_be`, which asserted the
      false positive was present; rule 7 replaced it with the correct expectation, ignored.
      Its live control, `..._is_well_formed_once_the_loop_is_known`, must stay green throughout
- [x] a playbook-local `item` with no loop anywhere is still reported
