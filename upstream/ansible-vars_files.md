# Upstream regression dossier — a missing `vars_files` file is silently ignored

Unlike `ansible-include_vars.md`, this is **not** a new issue to file: upstream already
knows. This records the verified history and repro (ansible-core 2.21.2, Homebrew ansible
14.2.0), plus a draft comment to bump the open issue.

**Upstream state:**

- [#80483](https://github.com/ansible/ansible/issues/80483) — filed 2023-04 by **sivel (core
  maintainer)**, diagnosing the chain below. Still open; labels `affects_2.16`,
  `affects_2.18`, `data_tagging`.
- [#81419](https://github.com/ansible/ansible/issues/81419) — user report of exactly this
  ("2.15: Missing vars_files no longer fails the playbook but silently continues"), closed
  as duplicate of #80483.
- [PR #80505](https://github.com/ansible/ansible/pull/80505) — sivel's fix ("Refactor
  loading of vars_files to simplify and properly implement expectations"), **approved by
  bcoca and s-hertel, then went stale and was closed unmerged.**

## How the error was lost (from ansible/ansible git history)

Pre-2.15, `VariableManager.get_vars` ended the first-found loop with a gated raise
(`lib/ansible/vars/manager.py`, devel @ 2022-12-27):

```python
    except AnsibleFileNotFound:
        # we continue on loader failures
        continue
    except AnsibleParserError:
        raise
else:
    # if include_delegate_to is set to False or we don't have a host, we ignore the missing
    # vars file here because we're working on a delegated host or require host vars
    if include_delegate_to and host:
        raise AnsibleFileNotFound("vars file %s was not found" % vars_file_item)
```

Fragile even then — it fired only on per-host calls, never during the pre-play loads. Then:

1. **2023-03-23, `42355d181a`, PR #80171** ("Do not double calculate loops and
   `delegate_to`", 2.15): flipped `get_vars(include_delegate_to=True)` to `False` as part
   of the delegated-vars refactor. The vars_files code was untouched, but its gate
   `include_delegate_to and host` became always-false — **the raise turned unreachable**.
2. **2024-05-17, `c5114e1819`, PR #83259** ("Remove deprecated
   `VariableManager._get_delegated_vars`", 2.18): deleted the dead `for/else` raise
   entirely, leaving the loop's "If none are found, we raise an error" comment orphaned —
   it still sits in 2.21.2 (`vars/manager.py:342-344`) above code that raises nothing.

## Live repro (2.21.2)

```yaml
- hosts: localhost
  gather_facts: false
  vars_files:
    - vars/real.yml                 # exists
    - vars/definitely_missing.yml   # does not
    - - vars/nope-a.yml             # first-match group, none exist
      - vars/nope-b.yml
  tasks:
    - debug: { msg: "real_var={{ real_var }}" }
```

- Run: **`ok=1 failed=0`, zero warnings.** The missing plain entry and the all-missing
  group are both silently ignored.
- Using a variable the missing file should define fails much later, at the task:
  `'setting_from_missing_file' is undefined` — pointing at the task, not the vars_files
  line.
- The missing filename appears in output only at `-vvvvv`, as the loader's
  `looking for "vars/definitely_missing.yml" at ...` probe lines. Default, `-v`, and
  `-vvv` show nothing.

## Draft comment for #80483

> Still present in ansible-core 2.21.2: a missing `vars_files` entry (plain or every
> alternative of a first-found list) is silently ignored — `ok=1 failed=0`, no warning,
> the filename appears only in `-vvvvv` loader probes. The variables are simply never
> set, and the failure surfaces later as an unrelated-looking undefined-variable error on
> whichever task first uses one.
>
> For anyone tracing it: the pre-2.15 raise was gated on `include_delegate_to and host`;
> #80171 flipped that default to False (making the raise unreachable), and #83259 removed
> the dead code — the loop's "If none are found, we raise an error" comment in
> `vars/manager.py` still describes the removed behavior. Would a rebase of #80505 (which
> had two approvals) be welcome?
