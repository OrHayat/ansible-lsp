# T-173 — Expand inventory_hostname when the play names a literal host

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P3       | S    | —          |

## Problem

```yaml
- hosts: server1                                   # inventory_hostname is provably "server1"
  tasks:
    - debug: msg="{{ hostvars[inventory_hostname].x }}"
```

T-171 made `hostvars['web01']` jump to `host_vars/web01.yml` by matching the literal key to
the filename. The magic-variable spelling of the same thing resolves to nothing, even when
the play states the host outright one line up.

Corpus, counting `hosts:` values across ~320 plays:

| `hosts:` value            | count | knowable                        |
| ------------------------- | ----- | ------------------------------- |
| `localhost`               | 109   | **yes** — literal single host   |
| `server1`                 | 35    | **yes**                         |
| `lustre_servers`            | 130   | no — a group, needs T-062       |
| `{{ ... }}` (various)     | ~30   | no — T-034                      |

So roughly 45% of plays name a host that needs no inventory to resolve, and
`host_vars/localhost.yml` already exists in `demo/`.

## Approach

Narrow: `hosts:` a single literal token that is not a group name we would have to resolve.
Then `hostvars[inventory_hostname]` (and `hostvars[inventory_hostname]['x']`) resolves the
same way T-171's literal key does, via `host_vars_file`.

The hard part is knowing a bare token is a *host* and not a *group* — `lustre_servers` looks
identical to `server1` in the playbook. Without inventory we cannot tell, so the honest
signal is the filesystem: resolve only when `host_vars/<token>.yml` exists. A group whose
name matches a `host_vars/` file would be a mis-jump, which is worth a check before landing.

Comma lists and `:` patterns (`hosts: a,b`) name several hosts, so `inventory_hostname` is
not one value — skip them.

## Not this ticket

This does **not** unblock T-172. That rule is stuck on "can inventory define this name",
not on "which host is this" — knowing the host is `server1` still says nothing about what
`inventory.ini` sets for it. Expanding the name is navigation only.

## Done when

- [ ] `hosts: server1` + `hostvars[inventory_hostname]` jumps to `host_vars/server1.yml`
- [ ] both subscript spellings resolve, matching T-171
- [ ] a group-valued `hosts:` with no matching `host_vars/` file stays silent
- [ ] a comma list or pattern stays silent
- [ ] a templated `hosts:` stays silent (T-034's, not this one's)
- [ ] a demo row, pinned by a test
