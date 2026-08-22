# T-204 — An Ansible upgrade mid-session is invisible until restart

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P3       | M    | —          |


## Problem

`AnsibleInstall::detect` runs exactly once, in `Backend::startup` (`main.rs:1237`), which is
spawned once from `initialized` (`main.rs:2771`). Nothing re-detects. So every field of the
detected install is fixed for the life of the server:

| field | what goes stale after `pip install -U ansible-core` |
| ----- | --------------------------------------------------- |
| `version` | version-gated rules keep judging against the old core |
| `package_dir` | usually survives an in-place upgrade, but not a venv switch |
| `collection_roots` | a newly installed collection is not offered |
| `builtin_routing` | the 2.10 split table - renames added in the new release do not resolve |
| `python` | `ansible_playbook_python` hovers keep the old interpreter |

Found while reviewing T-201 box (6), where a code comment claimed core's routing table
"cannot change while the server runs". It can - upgrading Ansible rewrites it. What is true is
that we never look again, and that is a different statement about a different thing.

Nothing here is new with box (6): the install was already detected once behind a `OnceLock`
before T-201 moved it onto `State`. Box (6) did close one accidental window, and that is worth
recording as a small loss rather than left to be discovered: the routing table used to be
parsed lazily on the first bare-name miss, so an upgrade landing between startup and that miss
*was* picked up. It is now read during detection. That was traded knowingly - the lazy parse
happened on the message pump, which is the freeze T-084 exists about - but it means the
"upgrade during a session" hole is now uniform rather than accidental in one field's favour.

Not yet shown to bite a user. An editor session outliving an Ansible upgrade is plausible
(a `pip install -U` in the integrated terminal) but nobody has reported it, and the failure is
a *stale* answer rather than a fabricated one: the tool describes the install it detected, which
did exist. Priority reflects that; measure before raising it.

## Approach

Options, in rough order of cost:

- **Say so.** The status bar already reports the detected install (T-062's argument: a silently
  chosen thing reproduces the ambiguity the tool exists to remove). It could carry the detected
  version, so a stale answer is at least visible and "restart the server" is an obvious move.
  Cheapest, and honest.
- **Re-detect on demand.** A command - "Ansible: re-detect install" - that re-runs detection on
  a blocking task and replaces `State::install`. The value is already owned and swappable since
  T-201 box (4); this is a `Mutex` write and a re-publish, not a redesign.
- **Watch the install.** Re-detect when `package_dir` or its `config/ansible_builtin_runtime.yml`
  changes on disk. Related to [[T-012]] (file watcher and precise invalidation), but a different
  tree: T-012 watches the *workspace*, and this is outside it. Do not grow T-012 with it.

Whichever is chosen, the caches keyed on the old install have to go with it - `State::var_cache`
at minimum, since module resolution feeds the variable index.

## Done when

- [ ] measured: does a real upgrade mid-session actually produce a wrong answer, and which
      surface shows it first - a module that fails to resolve, or a version-gated rule
- [ ] the priority is adjusted to whatever that measurement says, rather than left at P3
      because the failure sounded unlikely
- [ ] whichever option is chosen is implemented, and the caches derived from the old install
      are dropped with it
- [ ] the comment on `AnsibleInstall::builtin_routing` is updated to point here instead of
      describing the limitation inline
