# T-096 — project_root stands in for the playbook dir

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | bug  | P1       | M    | T-090 | —          |

## Symptom

For the common layout — `ansible.cfg` at the repo root, playbooks under `playbooks/` — we
resolve files Ansible cannot find *and* miss files it can. Wrong in both directions, so the
diagnostic is unreliable in either polarity.

## Cause

`path_dwim_relative` (`parsing/dataloader.py:279-331`) builds seven candidates from the
*playbook's* directory: `loader.get_basedir()`, set to `dirname(playbook_file)`
(`playbook/__init__.py:56-62`) and re-based per import (`dataloader.py:224-228`). We
substitute `project_root`, the ancestor holding `ansible.cfg`, which is not an Ansible concept.

- a file at `<root>/x.yml` resolves here and fails for Ansible
- a file at `<root>/playbooks/tasks/x.yml` is Ansible's candidate 6 and invisible to us

Candidate 4 is `$CWD/tasks/<src>` — `unfrackpath` with no basedir resolves against
`os.getcwd()` (`utils/path.py:47-48`). Resolution genuinely depends on the invoking
directory, so a `Missing` verdict is never provably right. Worth a comment in the code even
though we cannot model it.

## Approach

Track the playbook directory per file — the entry playbook for a play, the importing
playbook's dir for anything reached through `import_playbook` — and use it where we currently
use `project_root`. Keep `project_root` as the workspace scan boundary only.

## Done when

- [ ] the base for task-file resolution is the playbook dir, not the repo root
- [ ] a fixture with `ansible.cfg` at root and playbooks in a subdir pins both directions
- [ ] the CWD-dependent candidate is documented as a reason we cannot be certain
- [ ] the corpus scan warning count is unchanged or explained
