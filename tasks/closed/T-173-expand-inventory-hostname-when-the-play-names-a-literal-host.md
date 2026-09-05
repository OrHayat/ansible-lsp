# T-173 — Expand inventory_hostname when the play names a literal host

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | task | P3       | S    | —          |

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
| `app_servers`            | 130   | no — a group, needs T-062       |
| `{{ ... }}` (various)     | ~30   | no — T-034                      |

So roughly 45% of plays name a host that needs no inventory to resolve, and
`host_vars/localhost.yml` already exists in `demo/`.

## Approach

Narrow: `hosts:` a single literal token that is not a group name we would have to resolve.
Then `hostvars[inventory_hostname]` (and `hostvars[inventory_hostname]['x']`) resolves the
same way T-171's literal key does, via `host_vars_file`.

The hard part is knowing a bare token is a *host* and not a *group* — `app_servers` looks
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

- [x] `hosts: server1` + `hostvars[inventory_hostname]` jumps to `host_vars/server1.yml`
- [x] both subscript spellings resolve, matching T-171
- [x] a group-valued `hosts:` with no matching `host_vars/` file stays silent
- [x] a comma list or pattern stays silent
- [x] a templated `hosts:` stays silent (T-034's, not this one's)
- [x] a demo row, pinned by a test

## Landed

`condition::hostvars_magic_keys` / `hostvars_magic_key_at` find the `hostvars[inventory_hostname]`
reads (the literal scan and this one now share `hostvars_subscripts`); `ast::literal_host_at`
says whether the play containing a byte names one bare host; `Backend::expanded_host` turns
that into the host the read consults, and both `host_key_defs_at` and `host_key_links` go
through it, so the click and the paint cannot disagree (rule 3). The demo row is the second
play in `demo/hostvars.yml`, pinned in `hostvars_reads_navigate_where_visible_and_warn_where_not`.

### The group check the Approach asked for, measured

On 2.21.2 with `[g]` / `h1` and both `host_vars/g.yml` and `host_vars/h1.yml` present:

| `hosts:` | `hostvars[inventory_hostname].x` reads |
| --- | --- |
| `localhost` (file present) | `host_vars/localhost.yml`, both `.x` and `['x']` |
| `h1` | `host_vars/h1.yml` |
| `g` | **`host_vars/h1.yml`** — `g.yml` is never consulted |

So the filesystem signal alone *would* mis-jump on a group with a same-named file. The
inventory now has the last word when it can be read: a bare name it does not list as a host
(nor `add_host` creates) is declined. When no inventory resolves, or it is dynamic, the
`host_vars/` file is the only signal left and a same-named group file is the accepted edge —
asserted as such in `a_pinned_inventory_hostname_jumps_only_for_a_host_the_play_provably_names`,
alongside the control that jumps.

The demo row itself was run: the second play prints `ap-south-1` twice from
`host_vars/localhost.yml`.
