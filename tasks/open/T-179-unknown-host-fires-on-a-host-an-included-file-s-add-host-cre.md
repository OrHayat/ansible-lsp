# T-179 — unknown-host fires on a host an included file's add_host creates

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P1       | M    | —          |

## Symptom

`unknown-host` (T-062) is an ERROR reading "No host `x` in the inventory, so this read fails
at runtime". It goes quiet when the file calls `add_host`, because that invents hosts no
inventory lists. The escape reads **one file's** parse, so an `add_host` in an imported task
file does not reach it, and the playbook gets a red error on code that runs clean.

Measured on 2.21.2, with both controls in the same fixture so the probe could report either
way:

| file                                            | ansible          | we say  |
| ----------------------------------------------- | ---------------- | ------- |
| `import_tasks` a file whose `add_host` makes the host | `ok=2 failed=0` | **1 ERROR** |
| control: the same `add_host` written inline      | runs             | 0 — quiet |
| control: a host nothing anywhere creates         | fatal            | 1 ERROR |

The inline control is what localises it: the escape works perfectly when it can see the task.
The ghost control is what proves the rule was alive rather than switched off by the fixture.

P1 by the board's own test — the tool lies, and it lies in the loudest register it has. The
message asserts the read "fails at runtime" about a play that exits 0, and this is an ERROR
rather than a warning precisely because it was meant to be provable.

Fixture that produced it: `ansible.cfg` naming an ini inventory with one host, a playbook
importing `tasks/create.yml`, and that file adding `thathost`. A resolved inventory is
required to reproduce — with none, `vars::inventory_hosts` returns `None` and everything is
silent anyway.

## Cause

`unknown_host_diagnostics` (`main.rs:693`):

```rust
if vars::calls_add_host(&a.nodes) {
    return Vec::new();
}
```

`a.nodes` is the analysed file's own parse (`Analysis.nodes`, `main.rs:315`). `calls_add_host`
is deliberately generous *within* a file — any key or value whose last dotted segment is
`add_host` counts — but generosity inside one file does not cross an `import_tasks` edge.

The T-062 comment at `main.rs:678-680` states the intended trade: "an escape that
over-matches only ever costs a missed report, while one that under-matches costs a false
error." The escape under-matches here, so the stated trade is not the one being made.

## Fix

Ask the question of the resolved graph rather than the file. The include/import edges are
already resolved for this file (`Analysis.refs`, and `include_targets` already reaches target
parses through the scan cache for T-110), so the reachable set is available at the same point
the diagnostic runs.

**Do not stop at silencing the file.** The obvious repair — "any `add_host` anywhere
reachable silences the rule" — is correct in its error direction but throws away a rule that
can still answer. In the fixture above the whole chain is literal: `import_tasks` names a
file, and that file's `add_host` names `thathost`. Both are readable, so the honest answer is
not "I give up on this file", it is **`thathost` is a host**:

| read                    | with the names added        |
| ----------------------- | --------------------------- |
| `hostvars['thathost']`  | known, created by `tasks/create.yml` — silent |
| `hostvars['ghost']`     | nothing creates it — still ERROR |

Adding names can only ever remove errors, so that half is safe on its own: an `add_host`
under a `when:` that never runs would leave the host uncreated and the read genuinely
broken, and we would stay quiet about it. A missed report, which is the trade already made.

**Completeness is what decides between the two tiers**, and it is where this gets its teeth:

- a **templated host name** — `name: "{{ item }}"` over a `k8s_info` result, which is the
  case T-177 came from — yields no name to add
- a **dynamic edge** — `include_tasks: "{{ kind }}.yml"` — yields no file to read

If either appears anywhere in the reachable set, the set of created hosts is unknowable and
the rule must go silent for the file. Keeping it live on a partial set is precisely the
under-matching that caused this bug: it would fire on a host a templated `add_host` created.

So: walk the reachable set; if every edge resolved **and** every `add_host` name is literal,
add those names and keep the rule live; otherwise silence. `vars::inventory_hosts` already
returns `Option` for exactly this distinction (T-062) — unknowable is a third answer — so the
shape is in place and this is a third contributor to the same `Option`.

Two things to settle before writing it, each with a probe:

- **Which direction the edges count.** A role's `tasks/main.yml` calling `add_host` should
  count for the playbook that uses the role. The reverse — a playbook's `add_host` reaching an
  unrelated task file that happens to be included elsewhere — is not obviously wanted.
- **Cost.** This runs per-file on every keystroke's diagnostics pass. Walking the include
  graph per diagnostic may need the same caching `include_targets` uses; measure before
  assuming it is free (T-131).

## Done when

- [ ] the three-file fixture above is a demo fixture, and the imported-`add_host` row is
      silent while both controls keep their current verdicts
- [ ] **a literal imported `add_host` name is added to the host set, not merely silenced** —
      asserted by a `hostvars['ghost']` read in the *same file* still firing. Without that
      row a "silence the whole file" implementation passes every other box here, which is
      the fixture's easy case doing the work instead of the rule.
- [ ] a **templated** imported `add_host` name (`name: "{{ item }}"`) silences the file
      instead, and the `ghost` read beside it goes quiet too — the set is unknowable, so the
      rule must stop answering rather than answer from a partial set
- [ ] a role's `add_host` counts for a playbook that uses the role, asserted separately from
      the `import_tasks` case
- [ ] `include_tasks` (dynamic) asserted as well as `import_tasks`, both for a literal
      filename and for a templated one — a templated edge is an unknowable set
- [ ] the reachable-set walk is measured against the diagnostics pass, not assumed cheap
- [ ] corpus gate: `unknown-host` hits on `~/app/ansible` can only fall, and any that
      disappear are confirmed `add_host`-created
