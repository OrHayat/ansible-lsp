# Upstream issue against ansible/ansible — a relative role name resolves against the process CWD

Already filed, by someone else: [#87100](https://github.com/ansible/ansible/issues/87100)
"Role search path implicitly includes working directory of ansible-playbook command"
(2026-06-11, open, `bug` `has_pr` `needs_verified`), with
[PR #87268](https://github.com/ansible/ansible/pull/87268) open against it (`needs_revision`,
a reviewer asked for integration tests). Nothing here needs filing. This dossier records our
own measurement, so the finding has one home in this repo and T-067 can cite it.

Measured on ansible-core **2.21.2** under WSL, from `/tmp` — a probe run under `/mnt/c` has
its `ansible.cfg` ignored as world-writable and would not measure the same thing.

## Issue 1 — the role-name-as-path fallback resolves a relative name against the shell's CWD, and the error omits it

**Component:** `lib/ansible/playbook/role/definition.py`, `_load_role_path`

**Summary.** After the four listed roots (`<playbook_dir>/roles`, `roles_path`, the dependency
basedir, `<playbook_dir>`) miss, the loader tries the name itself as a path:

```python
# if not found elsewhere try to extract path from name
role_path = unfrackpath(role_name)
```

`unfrackpath` ends in `os.path.abspath`, so a *relative* name is joined onto the process CWD.
The documented form is an absolute path (`role: '/path/to/my/roles/common'`,
`playbooks_reuse_roles`); the relative case falling onto CWD contradicts
[`playbook_pathing`](https://docs.ansible.com/ansible/latest/playbook_guide/playbook_pathing.html),
which says verbatim: "Ansible does not search for local files in the current working directory;
in other words, the directory from which you execute Ansible."

**Reproduction** (`scratchpad/t067_role_name_as_path_probe.sh`): `roles: [shared/myrole]` in
`playbooks/site.yml`, the only copy of the role at `<project>/shared/myrole`.

| Run from | Result |
| -------- | ------ |
| the project root | ran |
| `/tmp` | `was not found` |
| `playbooks/` | `was not found` |
| absolute `roles: [<project>/shared/myrole]`, from `/tmp` or the root | ran — the control |

Same files, same command, only the shell's directory changed. And the failure lists
`playbooks/roles : ~/.ansible/roles : /usr/share/ansible/roles : /etc/ansible/roles : playbooks`
— the CWD candidate, the one that decides, is not among them.

**The proposed fix** (#87268) passes `basedir=self._loader.get_basedir()`, anchoring a relative
name to the playbook directory. Measured by applying it to a copy of 2.21.2
(`scratchpad/t067_pr87268_probe.sh`), same fixture:

| Run from | before | after |
| -------- | ------ | ----- |
| the project root | ran | not found |
| `/tmp` | not found | not found |
| `playbooks/` | not found | not found |
| absolute path, from `/tmp` | ran | ran |

The answer stops depending on the shell. For a relative name the patched candidate is
`<playbook_dir>/<name>` — identical to the fourth root already in the list — so for relative
names the fallback becomes a duplicate check.

What the fallback is actually *for* is `~` and `$VAR`. `unfrackpath` expands both before it
joins, while the root loop joins first (`<root>/~/roles/x`, where `~` no longer leads and never
expands). Measured with three builds (`scratchpad/t067_fallback_shapes_probe.sh`), cwd = the
project root:

| Name | 2.21.2 | #87268 | fallback deleted |
| ---- | ------ | ------ | ---------------- |
| `shared/myrole` | found (via CWD) | — | — |
| absolute path | found | found | found |
| `~/t067roles/homerole` | found | found | — |
| `$HOME/t067roles/homerole` | found | found | — |

An absolute name never needed the fallback: `os.path.join(root, "/abs")` is `"/abs"`, so the
first root already finds it. The PR keeps the one thing only the fallback does and removes the
accident; the redundant check for plain relative names is harmless.

## Why this repo cares

T-067 must decide what to do with a role reachable only through this fallback. It cannot be
modelled — it depends on the operator's shell — so such a role stays unresolved. Upstream
calling it a bug means that policy is not an under-approximation to apologise for: it is the
behaviour ansible is moving to. Nothing needs adding when #87268 lands either: the anchored
form is `<playbook_dir>/<name>`, which T-067's list already has as its fourth root.

The `~`/`$VAR` rows are a separate thing we must model regardless: they resolve in every build,
so a role named `~/roles/x` is valid, and a resolver that only joins the name onto its roots
reports it missing.
