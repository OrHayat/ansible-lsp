# Upstream issue to file against ansible/ansible — `hostvars` membership and iteration disagree

Not filed yet. Measured on ansible-core **2.21.2**; the split below is structural, not a
recent regression.

## Issue 1 — `HostVars` declares `Mapping` but `in` and iteration use different sources of truth

**Component:** `lib/ansible/vars/hostvars.py`

**Summary.** `HostVars` is a declared `collections.abc.Mapping` (`hostvars.py:35`), which
requires `__contains__` to agree with `__iter__` and `__getitem__`. It does not:

```python
def __contains__(self, item: object) -> bool:
    # does not use inventory.hosts, so it can create localhost on demand
    return self._inventory.get_host(item) is not None      # :69-71

def __iter__(self) -> t.Iterator[str]:
    yield from self._inventory.hosts                        # :73-74

def __len__(self) -> int:
    return len(self._inventory.hosts)                       # :76-77
```

`get_host` materialises the implicit localhost on demand; `inventory.hosts` does not contain
it. So for an inventory that never mentions localhost:

```yaml
- hosts: webservers          # inventory: one host, node1
  tasks:
    - debug:
        msg:
          - "'localhost' in hostvars    = {{ 'localhost' in hostvars }}"
          - "hostvars | list            = {{ hostvars | list }}"
          - "'localhost' in (hostvars|list) = {{ 'localhost' in (hostvars | list) }}"
          - "hostvars['localhost'] ...  = {{ hostvars['localhost'].inventory_hostname }}"
```

```
'localhost' in hostvars        = True
hostvars | list                = ['node1']
'localhost' in (hostvars|list) = False
hostvars['localhost'] ...      = localhost
```

Membership says yes, iteration says no, and the subscript works. `len()` follows iteration,
so a mapping reports a length that excludes a key it confirms it contains.

**Why this matters.** The two idioms for "every host" are `{% for h in hostvars %}` and a
`'name' in hostvars` guard, and they are routinely used together — building an `/etc/hosts`,
a cluster peer list, a monitoring target file. A guard that passes and a loop that skips the
same host produce a rendered file missing an entry, with no error anywhere. The failure is a
wrong config file, not a traceback.

It also breaks the ordinary expectation that `dict(hostvars)`, `hostvars.keys()` and
`in` describe one set of keys, which is the contract `Mapping` exists to promise.

**Distinct from Issue 2 below.** A host in *neither* view does fail, loudly; that failure is
correct and only its wording is at fault. Issue 1 is the contract violation — a key that is
present by one method and absent by another.

**Either resolution closes it,** and the choice is upstream's:

- `__iter__`/`__len__` also materialise the implicit localhost, matching `__contains__`; or
- `__contains__` consults `inventory.hosts`, so `in` matches iteration and the implicit
  localhost is reachable only by explicit subscript.

The second is the smaller behaviour change and keeps "loops over hostvars" meaning "hosts in
this run". Either way the three methods should share one source of truth.

## Issue 2 — `hostvars['name']` for an unknown host reports the expression, not the reason

**Component:** `lib/ansible/vars/hostvars.py`

**Summary.** `raw_get` knows exactly why the lookup failed and does not say:

```python
host = self._inventory.get_host(host_name)

if host is None:
    from ansible._internal._templating import _jinja_bits
    return _jinja_bits._undef(f"hostvars[{host_name!r}]")      # :50-55
```

The whole error a user sees:

```
[ERROR]: Task failed: Finalization of task args for 'ansible.builtin.debug' failed:
         Error while resolving value for 'msg': hostvars['web0143']
```

Everything before the final colon is plumbing; the explanation is the expression echoed
back. Nothing states that `web0143` is not a host. A reader reasonably concludes the
*variable* is missing and goes looking in `group_vars/`, `host_vars/` or the role — the one
place the answer is not.

**Why this is a fix and not a wish.** `_undef` exists to carry exactly this:

```python
def _undef(hint: str | None = None) -> UndefinedMarker:
    """...optionally with a custom hint."""              # _jinja_bits.py:882
```

The hint is free-form and surfaces verbatim as the message. Returning a Marker rather than
raising is deliberate and correct — it is what lets `hostvars['x'] | default(...)` and
`is defined` work — so the deferral is not the problem. Only the hint's content is, and
`host is None` is in scope at that line:

```python
return _jinja_bits._undef(f"hostvars[{host_name!r}] — no host by that name in the inventory")
```

One line, no behaviour change, no new failure mode. The surrounding machinery is otherwise
good: the error already carries file, line, column and a `<<< caused by >>>` chain, which is
what makes the missing reason conspicuous rather than merely terse.

## What we do about it

Nothing to work around — we do not evaluate `hostvars` at edit time. It matters here because
T-062 will enumerate hosts to diagnose `hostvars['name']` for a host no inventory has, and
this is why `localhost` must never be flagged by that rule: it is a member without being an
element, so "not in the inventory" is not the same question as "not a valid key".
