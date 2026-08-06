# T-101 — Dynamic includes reject keywords imports accept

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P1       | S    | T-099 | —          |

## Problem

```yaml
- include_tasks: x.yml     # AnsibleParserError at load
  become: true
- import_tasks: x.yml      # completely fine
  become: true
```

`TaskInclude.VALID_INCLUDE_KEYWORDS` is 16 names — `action, args, collections, debugger,
ignore_errors, loop, loop_control, loop_with, name, no_log, register, run_once, tags,
timeout, vars, when` (`playbook/task_include.py:42-44`); `HandlerTaskInclude` adds `listen`
(`handler_task_include.py:27`). Anything else on a **dynamic** include is fatal.

The check runs only when `ds['action'] in C._ACTION_ALL_INCLUDE_ROLE_TASKS`
(`task_include.py:90-97`), and that constant is `include_role` + `include_tasks`
(`constants.py:45`) — so the identical keyword on `import_tasks`/`import_role` is legal.

So `become`, `delegate_to`, `environment`, `notify`, `until`, `retries`, `changed_when`,
`failed_when`, `check_mode`, `throttle`, `connection`, `remote_user`, `module_defaults`,
`any_errors_fatal` and `ignore_unreachable` each fail on one form and pass on the other.
`include_tasks`' own docs say the do-until loop is unsupported (`modules/include_tasks.py:34`).

Also here, same shape: `apply:` is valid only on `include_tasks`/`include_role` and must be a
dict (`task_include.py:67-83`), and its contents are loaded as a Block at *expansion* time
(`:106-124`) — so a bad key inside `apply:` is a **run-time** parser error that
`--syntax-check` never reaches. That one is genuinely new coverage rather than earlier
coverage.

## Approach

Two sets keyed on include-vs-import. Zero false positives — the sets are closed and the
severity is unambiguous.

## Done when

- [ ] a non-whitelisted keyword on `include_tasks`/`include_role` is an ERROR
- [ ] the same keyword on `import_tasks`/`import_role` is not
- [ ] `apply:` on an import is an ERROR, and its contents are checked as a Block
- [ ] the whitelist is derived from one table, not repeated per action
