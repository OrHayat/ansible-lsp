# T-165 — A loop on meta: is silently discarded

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | S    | T-099 | —          |

## Problem

```yaml
- name: meta with a loop
  meta: noop
  loop: [a, b, c]        # runs zero times, says nothing

- name: control
  debug: {msg: "{{ item }}"}
  loop: [a, b, c]        # runs three times
```

Measured on 2.21.2: the first task's `loop:` produces **no iterations at all** — the TASK
header prints and nothing follows — while the identical loop on the control task runs three
times. No error, no warning, exit 0.

The keyword is legal, spelled right, and in a legal position, so `attributes` and
`placement` both stay quiet. Same family as `dead-loop-control` (T-155) and
`shadowed-loop`: a keyword whose value is discarded while the play runs clean.

## Why it is safe to flag all ten subactions

The docs hedge — `bypass_task_loop: partial`, "Most of the subactions ignore the task loop,
see the description above for each specific action for the exceptions"
(`modules/meta.py:61-62`) — but the source has no such exceptions:

- every meta action is short-circuited in the strategy (`strategy/__init__.py:848`), ahead
  of any loop expansion, so the discard is not per-subaction
- `_get_meta` reads `self.args.get('_raw_params')` directly, with the comment "meta
  currently does not support being templated, so we can cheat" (`task.py:237-243`) — the
  subaction name therefore cannot be `{{ item }}`
- `meta` accepts no other args (`choices: [clear_facts, clear_host_errors, end_host,
  end_play, flush_handlers, noop, refresh_inventory, reset_connection, end_batch,
  end_role]`, `meta.py:48`)

So there is nothing on a `meta` task a loop could vary. The doc fragment's "exceptions"
wording is about `bypass_host_loop`, which is genuinely per-subaction; the task-loop line
appears to have inherited the hedge. Confirm against a second subaction before shipping,
but do not build a per-subaction allowlist off the doc text.

Treat the docs as untrusted here generally: the *import* fragment on the same attribute
(`doc_fragments/action_core.py:48`) claims "the task itself is not looped, but the loop is
applied to each imported task", and 2.21.2 in fact raises `You cannot use loops on
'import_tasks' statements` — which is T-110's row and already replicated.

## Approach

A co-occurrence shape test in `placement.rs`, beside `dead-loop-control`: action is `meta`
(any of the three core spellings) **and** the node carries `loop:`/`loop_with`/`with_*`.
WARNING, its own rule id — ansible-core is silent, so this is ours.

The message should say the loop runs zero times, not that it is "unused": the author's
mental model is that the task runs once per item, and the truth is that it runs once.

## Done when

- [ ] `meta: noop` with a `loop:` warns, naming that the task runs once regardless
- [ ] the `with_*` spellings warn identically
- [ ] a `meta:` with no loop, and a loop on any non-`meta` action, stay silent
- [ ] behaviour confirmed against a second subaction, so the rule is not pinned on `noop`
- [ ] its own rule id, `# noqa`-suppressible per T-010, WARNING severity
- [ ] a demo fixture carries the flagged and unflagged forms
