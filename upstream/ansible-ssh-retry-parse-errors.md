# Upstream issue to file against ansible/ansible — `_ssh_retry` retries ssh's own command-line errors, which cannot succeed

Not filed yet. Measured on `origin/devel` at `3827d66` (2026-09-04) under Python 3.14 and
OpenSSH 10.3p1. Once `ansible-ssh-args-worker-crash.md` lands, a malformed option reaches this
path instead of killing the worker, which is what makes the cost below worth fixing.

## Issue 1 — an exit 255 from ssh's argument parsing is retried like a connection failure

**Component:** `lib/ansible/plugins/connection/ssh.py`

**Summary.** `_ssh_retry` (`ssh.py:548-612`) wraps every ssh, scp and sftp call.
`_handle_error` turns any exit 255 into `AnsibleConnectionFailure` (`ssh.py:522-540`), and the
decorator retries that up to `reconnection_retries` times, sleeping `2**attempt - 1` seconds,
capped at 30, between attempts (`ssh.py:599-601`). The option's description says what the
retry is for: "Ansible retries connections only if it gets an SSH error with a return code of
255" (`ssh.py:208-212`). But ssh also exits 255 when it rejects its own command line, before
any connection is attempted, with stderr beginning `command-line line 0:`. That verdict is a
function of the arguments alone, so every retry gets the same answer and the pauses are pure
cost.

**Reproduction:** an option value ssh does not accept, with retries opted in:

```
$ ANSIBLE_SSH_RETRIES=5 ansible all -i 'somehost,' -m ping -e '{"ansible_ssh_extra_args": "-o RequestTTY=fable"}'
somehost | UNREACHABLE! => {
    "msg": "Task failed: Failed to connect to the host via ssh: command-line line 0: unsupported option \"fable\".",
    ...
```

Wall clock for that one host, same command, only the retry count varied:

| `ANSIBLE_SSH_RETRIES` | wall time | sleeping    | ssh's answer on every attempt                     |
| --------------------- | --------- | ----------- | ------------------------------------------------- |
| 0                     | 0.55 s    | 0 s         | `command-line line 0: unsupported option "fable".` |
| 3                     | 4.63 s    | 0+1+3 = 4 s | same                                              |
| 5                     | 27.3 s    | 1+3+7+15 = 26 s | same                                          |

At `-vv` each attempt logs `ssh_retry: attempt: N, ssh return code is 255`.

Control: the same retry count against a host that does not resolve, with no bad option, takes
27.0 s. That is the designed case — a resolution failure can change between attempts — and it
shows the cost is the retry loop, not the bad option. The difference is that a parse error
cannot change.

**Measured scope.** `reconnection_retries` defaults to 0 (`ssh.py:213`), so only users who
opted in pay this, and they pay it per host and per task.

**Expected.** No retry when the 255 came from ssh's own argument or configuration parsing. The
decorator already has a non-retryable path: `AnsibleAuthenticationFailure` is re-raised at
once "to prevent further retries" (`ssh.py:591-594`).

**Suggested fix.** In `_handle_error`'s 255 branch, when stderr starts with
`command-line line 0:` — the label ssh gives options that arrived by `-o`, present in both
messages above — raise a non-retryable subclass of `AnsibleConnectionFailure`, and have
`_ssh_retry` re-raise it the way it re-raises the authentication failure.

## Our side

No ticket filed. The value that triggers this is the same malformed `ssh_args` or
`ansible_ssh_extra_args` the other two ssh dossiers describe; a check on those values at edit
time would prevent all three.
