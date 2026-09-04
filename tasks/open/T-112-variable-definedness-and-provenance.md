# T-112 — Variable definedness and provenance

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | epic | P1       | L    | —          |

## Problem

Fifteen tickets, eight of them shipped, orbiting one question: **for a given `{{ name }}`,
which definition wins, and is there one at all?**

The shipped half built the index and the read paths — definitions (T-048), uses with accurate
spans (T-049), go-to-definition (T-050), hover with value and provenance (T-052, T-066), the
remaining sources (T-053), path expansion (T-056) and the cache that made it affordable
(T-055).

The open half is the *diagnostic*, and it is stuck on one measured fact: T-051's rule as
written produces **656 hits** on `~/app/ansible`, which is not a backlog of bugs, it is a rule
that does not know where variables come from. Everything still open is either a way to raise
that bar or a source the index cannot yet see:

- **T-062 is the unblock.** ini inventories and extension-less `group_vars`/`host_vars` are
  invisible today, and they are where a large share of the 656 are defined. It gates T-060 and
  T-061, and T-065 says to try it before settling for the interim fix.
- T-065 raises the bar from "unreachable from this file" to "absent from the workspace".
- T-060 narrows to near-misses; T-059 pushes the question to call sites; T-061 reframes the
  leftovers as the playbook's `-e` contract rather than errors.
- T-071 is the same machinery aimed at templated paths.

The grouping earns its place by making the sequence legible: **T-062 first**, then re-measure,
then decide whether T-065 is still needed. That ordering currently exists only inside the
individual files.

Precedence itself is settled and correct — `VarSource::precedence` (`vars.rs:63-77`) matches
`VariableManager.get_vars` order, including magic vars beating `-e` (`vars/manager.py:417`
then `:423`). Two gaps found in the 2.22 audit belong to children here, not to a new ticket:
`set_fact: cacheable: true` writes at **two** layers (`action/set_fact.py:55,59`), and
`group_vars/<name>/` as a directory silently shadows `group_vars/<name>.yml`
(`dataloader.py:470-491`) — the latter is T-062's to fix.

## Children

- [x] T-048 — Variable definition index (in-file + cross-file)
- [x] T-049 — Variable uses with byte-accurate spans
- [x] T-050 — Go-to-definition (and link colour) for variables
- [ ] T-051 — Variable definedness diagnostic
- [x] T-052 — Variable hover: where it's defined, and its value
- [x] T-053 — Remaining variable-definition sources: include_vars and role params
- [x] T-055 — Performance: cache the cross-file variable index; fix per-call line scans
- [x] T-056 — Expand known-value variables in templated paths (navigation only)
- [ ] T-059 — Call sites must satisfy the callee's required vars (`var-unpassed`)
- [ ] T-060 — `suspicious-var`: guarded, undefined, and one edit from a real name
- [ ] T-061 — `undeclared-var`: surface the playbook's required `-e` inputs
- [x] T-062 — Index ini inventories and extension-less group_vars/host_vars
- [ ] T-065 — `var-undefined`: raise the bar to workspace-wide absence (the 656 fix)
- [x] T-066 — Hover provenance breadcrumb for non-obvious definition routes
- [ ] T-071 — `unconstrained-path-var`: surface the value set a templated path implies
- [x] T-182 — Derive a variable's value domain from the assert that constrains it
- [ ] T-221 — Resolve a dotted access into a literal mapping variable
- [ ] T-222 — Four names ansible always provides are flagged as never defined: role_names, inventory_file, role_uuid, environment
- [ ] T-223 — Undefined softening is a substring match, so a name containing 'defined' or a lookalike guard silences a real undefined read

## Done when

- [ ] every child is closed or rejected
- [ ] `var-undefined` runs over `~/app/ansible` with a hit count small enough to read, and
      every remaining hit is a real fault or a documented concession
