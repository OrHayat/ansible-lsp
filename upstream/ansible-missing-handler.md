# Upstream issue to file against ansible/ansible — `ERROR_ON_MISSING_HANDLER` cannot fire on an unchanged task

Not filed yet. Read from ansible-core **2.22.0.dev0** (`lib/ansible/release.py:20`); the gate
below is long-standing and not a recent regression.

## Issue 1 — a `notify:` naming a handler that does not exist is silent unless the task reports `changed`

**Component:** `lib/ansible/plugins/strategy/__init__.py`, `lib/ansible/config/base.yml` (docs)

**Summary.** `ERROR_ON_MISSING_HANDLER` is documented as

> "Toggle to allow missing handlers to become a warning instead of an error **when
> notifying**." — `config/base.yml:1379`, default `True`

The implementation only checks when the task *also* reported `changed`
(`plugins/strategy/__init__.py:630`):

```python
for result_utr in result_utrs:
    if result_utr.notify and task_result.utr.changed:
        # only ensure that notified handlers exist, if so save the notifications ...
        for notification in result_utr.notify:
            ...
            if handler is Sentinel:
                msg = (
                    f"The requested handler '{notification}' was not found in either the main handlers"
                    " list nor in the listening handlers list"
                )
                if C.ERROR_ON_MISSING_HANDLER:
                    raise AnsibleError(msg)
                display.warning(msg)
```

The `raise` and the `display.warning` are both inside `and task_result.utr.changed`. So on a
converged host — the normal case for a re-run — a typo'd handler name produces no error, no
warning, and no `-v` output. The docs promise a check "when notifying"; the code performs one
when notifying **and** changing.

**Why this matters.** Handler names are strings matched exactly
(`plugins/strategy/__init__.py:506-510`), with no namespacing help beyond the `role : name`
aliases. A rename in `handlers/main.yml` that misses one `notify:` in a large role is
undetectable by any idempotent run, by `--syntax-check`, and by `--check`. It surfaces only on
the first run where that specific task happens to change — which may be months later, on a
host nobody is watching, and the play then silently does not restart the service it was
supposed to restart.

**Reproduction** (any recent ansible-core):

```yaml
# repro.yml
- hosts: localhost
  gather_facts: false
  tasks:
    - name: converged task, notifies a handler that does not exist
      ansible.builtin.file:
        path: /tmp/repro-marker
        state: touch
        modification_time: preserve
        access_time: preserve
      notify: definitely not a real handler
  handlers:
    - name: a real handler
      ansible.builtin.debug:
        msg: hello
```

First run: the file is created, the task is `changed`, and the play fails with
`The requested handler 'definitely not a real handler' was not found ...`.

Second run: the task is `ok`, and the play **succeeds silently**. The typo is still there.

```
$ ansible-playbook repro.yml     # run 1 -> fatal
$ ansible-playbook repro.yml     # run 2 -> ok=1  changed=0  failed=0
```

**Expected.** The existence of a notified handler does not depend on whether the notifying
task changed. Either check it unconditionally when `notify` is present, or document the
`changed` gate — the current text does not hint at it.

**Suggested fix.** Hoist the existence check out of the `changed` branch, keeping the
*notification recording* where it is:

```python
for result_utr in result_utrs:
    if result_utr.notify:
        for notification in result_utr.notify:
            if not any(self.search_handlers_by_notification(notification, iterator)):
                msg = f"The requested handler {notification!r} was not found ..."
                if C.ERROR_ON_MISSING_HANDLER:
                    raise AnsibleError(msg)
                display.warning(msg)
        if task_result.utr.changed:
            ...  # existing recording logic, unchanged
```

This changes behaviour for playbooks that currently pass with a broken `notify:` — which is
the point, and is why it likely wants a `changed_when`-style deprecation cycle rather than a
straight fix.

## Related

- `notify:` is `isa='list'` with no comma splitting (`playbook/notifiable.py:10`), unlike
  `tags` (`playbook/taggable.py:53-54`). So `notify: "restart a, restart b"` is **one**
  handler named `"restart a, restart b"` — which, combined with the above, is silent on every
  unchanged run. Worth mentioning in the same report; possibly its own issue.
- Duplicate handler names are not reported either: handlers are iterated reversed so "the last
  handler loaded with the same name wins" (`plugins/strategy/__init__.py:468-469`), and the
  earlier one is unreachable dead code.

## Our side

`T-028` (handler index) is the editor-side diagnostic for the same fault and does not depend
on this being fixed upstream.
