# Upstream issues to file against ansible/ansible — `RequestTTY` handling in the ssh connection plugin

Not filed yet. Measured on `origin/devel` at `3827d66` (2026-09-04) under Python 3.14 and
OpenSSH 10.3p1. Both findings are in the same predicate as the worker crash in
`ansible-ssh-args-worker-crash.md`, and the maintainer direction recorded there applies: the
predicate answers yes or no, and ssh is the one that reports malformed input.

## Issue 1 — `-o RequestTTY` with no value raises `IndexError`, reported as "list index out of range"

**Component:** `lib/ansible/plugins/connection/ssh.py`

**Summary.** `_is_tty_requested` splits each `-o` value into keyword and value and reads
`val[1]` whenever the keyword is `requesttty` (`ssh.py:1594-1601`):

```python
if '=' in arg:
    val = arg.split('=', 1)
else:
    val = arg.split(maxsplit=1)

if val[0].lower().strip() == 'requesttty':
    if val[1].lower().strip() in ('yes', 'force'):
        return True
```

A bare `-o RequestTTY` splits to one element, so the read raises `IndexError`, which surfaces
as a task failure whose only text is Python's.

**Reproduction:**

```
$ ansible all -i 'somehost,' -m ping -e '{"ansible_ssh_extra_args": "-o RequestTTY"}'
[ERROR]: Task failed: list index out of range
somehost | FAILED! => {
    "msg": "Task failed: list index out of range"
}
$ echo $?
2
```

Control: `-o RequestTTY=yes` on the same command gives the ordinary `UNREACHABLE!` for a host
that does not resolve, exit 4.

ssh's own verdict on the same input:

```
$ ssh -o RequestTTY somehost
command-line line 0: no argument after keyword "requesttty"
```

**Expected.** A valueless keyword counts as "no tty requested", and ssh reports it through the
existing exit-255 handling — the same shape the maintainer asked for on PR #87219.

**Suggested fix.** Guard the read:

```python
-            if val[0].lower().strip() == 'requesttty':
+            if len(val) == 2 and val[0].lower().strip() == 'requesttty':
```

## Issue 2 — `RequestTTY=true` is a tty to ssh but not to the predicate, so pipelining stays on while ssh allocates one

**Component:** `lib/ansible/plugins/connection/ssh.py`

**Summary.** The predicate accepts only `yes` and `force` (`ssh.py:1600`). ssh accepts `true`
as the same value as `yes`, measured through its own resolved configuration:

```
$ for v in yes true force no; do printf "%-6s -> " $v; ssh -G -o RequestTTY=$v somehost | grep ^requesttty; done
yes    -> requesttty true
true   -> requesttty true
force  -> requesttty force
no     -> requesttty false
```

The predicate, called directly on a plugin instance with `ansible_ssh_extra_args` set to each:

```
RequestTTY=yes   -> _is_tty_requested() = True
RequestTTY=force -> _is_tty_requested() = True
RequestTTY=true  -> _is_tty_requested() = False
RequestTTY=no    -> _is_tty_requested() = False
```

So with `RequestTTY=true`, `is_pipelining_enabled` (`ssh.py:1605-1615`) leaves pipelining on
— the exact case the override exists to prevent, per its own docstring ("ensure we don't
request a tty"). What a pipelined module then does over a tty was not measured; the finding
is that the predicate and ssh disagree about a value ssh accepts.

**Expected.** `true` counted like `yes`. ssh's parse is case-insensitive and the predicate
already lowercases, so the tuple is the only change.

**Suggested fix.**

```python
-                if val[1].lower().strip() in ('yes', 'force'):
+                if val[1].lower().strip() in ('yes', 'true', 'force'):
```

## Our side

No ticket filed. `RequestTTY=` takes a closed, four-value set, and the tool reads the two places
the value is written — `ansible.cfg` and the variable index — so a check is possible if it is
ever wanted.
