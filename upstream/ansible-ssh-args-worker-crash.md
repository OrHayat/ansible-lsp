# Upstream issue to file against ansible/ansible — a malformed `ssh_args` kills the worker process instead of failing the task

Filed as [PR #87219](https://github.com/ansible/ansible/pull/87219) (open, head `3b115058a7`),
being reworked to the maintainer direction recorded below. Measured on `origin/devel` at
`3827d66` (2026-09-04) under Python 3.14 and OpenSSH 10.3p1. The same function has two more
findings in `ansible-ssh-requesttty.md`, and the retry cost of the fixed behaviour is
`ansible-ssh-retry-parse-errors.md`.

## Issue 1 — `_is_tty_requested` lets argparse call `sys.exit()`, and `SystemExit` is not an `Exception`, so the worker dies

**Component:** `lib/ansible/plugins/connection/ssh.py`

**Summary.** The ssh connection plugin decides whether pipelining is safe by re-parsing its own
`ssh_args`, `ssh_common_args` and `ssh_extra_args` with a private `argparse.ArgumentParser`
(`ssh.py:663-665`). `_is_tty_requested` does the parse (`ssh.py:1579-1588`) and
`is_pipelining_enabled` calls it for every task (`ssh.py:1611`). The parser is built with
argparse's default `exit_on_error=True`, so an option with no value — a dangling `-o` — makes
argparse print usage and call `sys.exit(2)`. `SystemExit` derives from `BaseException`; the
worker's `except Exception` never sees it, and the worker process simply dies.

**Reproduction:**

```
$ ANSIBLE_SSH_ARGS="-o StrictHostKeyChecking=no -o" ansible all -i 'somehost,' -m ping
[ERROR]: Traceback (most recent call last):
  File ".../argparse.py", line 2047, in _parse_known_args2
  ...
argparse.ArgumentError: argument -o: expected one argument
  ...
  File ".../ansible/plugins/connection/ssh.py", line 1611, in is_pipelining_enabled
  File ".../ansible/plugins/connection/ssh.py", line 1588, in _is_tty_requested
  ...
SystemExit: 2

[ERROR]: A worker was found in a dead state
$ echo $?
1
```

58 lines, most of them argparse's traceback. The outcome is reported as a dead worker rather
than a failed task on the host, and nothing in it names `ssh_args`.

Control: the same command without the trailing `-o` gives the ordinary 11-line `UNREACHABLE!`
for a host that does not resolve, exit 4.

**Measured scope.** The three options are concatenated before the parse, so the same value
reaches the same line from any of them: `-e '{"ansible_ssh_extra_args": "-o"}'` produces the
identical 58-line dead-worker output, exit 1.

**Maintainer direction.** The first version of the PR caught the error inside the predicate
and raised a named failure. The maintainer (review of 2026-09-04) rejected that placement:

> The failure comes from `_is_tty_requested` which is supposed to be just a simple yes/no
> predicate. It should not be responsible for handling parsing errors. I think the right thing
> to do to fix the hard failure is to suppress the error (triggering the `sys.exit()`) and let
> the current option parsing error handling mechanism deal with the rest.

**Expected, measured under that direction.** With `ArgumentParser(exit_on_error=False)` and
`except argparse.ArgumentError: return False` in the predicate — patched in a scratch clone,
then reverted — the same command gives the ordinary 11-line `UNREACHABLE!`, exit 4, and ssh
itself names the problem through the existing exit-255 path (`_handle_error`,
`ssh.py:522-540`):

```
"msg": "Task failed: Failed to connect to the host via ssh: command-line line 0: no argument after keyword \"-o\""
```

The keyword ssh reports is `-o` because the dangling flag swallowed the next `-o` ansible
itself appends; the message is ssh's, not ansible's, which is the point of the direction.

**Suggested fix.** Exactly that two-line change, plus the changelog fragment and the
integration test in `test/integration/targets/connection_ssh/runme.sh` the PR already carries.

## Our side

No ticket filed. If one is ever wanted: `ssh_args` in `ansible.cfg` is a value the tool already
reads, and a trailing `-o` is as provable an error as a bad `keyword=`; `ansible_ssh_extra_args`
in inventory or `group_vars` is the same value in the variable index.
