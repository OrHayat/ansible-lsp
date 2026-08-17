# T-179 — unknown-host fires on a host an included file's add_host creates

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | bug  | P1       | M    | —          |

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

"Unknowable" here means *not readable today*, not unknowable in principle — do not write the
silence in as though the question were settled. A templated edge is often constrained a few
lines earlier: `include_tasks: "{{ kind }}.yml"` under an `assert: that: kind in ['x','y']`
has a closed domain and two readable targets. T-180 is that reader, and when it lands this
rule should ask it before giving up. Until then the give-up is correct, just wider than it
needs to be.

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

- [x] the three-file fixture above is a demo fixture, and the imported-`add_host` row is
      silent while both controls keep their current verdicts — `demo/unknown_host.yml` plus
      `demo/tasks/creates_host.yml`, asserted as the *exact* named set, not a count
- [x] **a literal imported `add_host` name is added to the host set, not merely silenced** —
      `buildbox` is silent and `web0143` in the same file still fires. Verified by disabling
      the union and watching the fixture report both, which is the shape a "silence the file"
      implementation would ship.
- [x] a **templated** imported `add_host` name (`name: "{{ item }}"`) silences the file
      instead, and the `ghost` read beside it goes quiet too, with a literal-name control in
      the same test proving the rule was live. The free-form `add_host: name=h` spelling is
      unknowable rather than ignored — T-177 records it as a known miss on the variable side,
      and a missed name here would be read as "no such host" and reported.
- [x] a role's `add_host` counts for a playbook that uses the role, asserted separately from
      the `import_tasks` case. Measured on 2.21.2 first: `roles:` runs before the play's
      tasks and `rolehost` reads back, while the negative control — an `add_host` in a file
      nothing includes — is fatal with the same bare message an unknown host gives. That pair
      is what settled "which direction the edges count": reachability outward, never a
      workspace scan.
- [x] `include_tasks` (dynamic) asserted as well as `import_tasks`, both for a literal
      filename and for a templated one — a templated edge is an unknowable set, via
      `SkipReason::Templated`, which `resolve` already reports and nothing was reading.
- [x] the reachable-set walk is measured against the diagnostics pass, not assumed cheap —
      and it was not. First cut: **4.42s** over the 759-file corpus against **140ms** for the
      whole of `diagnostics_of`, i.e. 31x the rest of the pass, because both host sets were
      computed before anything checked whether the file contains a `hostvars['literal']` at
      all. Moving that check first takes it to **248ms** vs 143ms. Nearly every file now exits
      before the walk, and the cost that remains is only on files that could actually produce
      a diagnostic.
- [x] corpus gate: **0 hits** before and after, so nothing fell and nothing appeared. The
      corpus never contained this bug's shape — the fixture had to be built — which is the
      argument for the demo fixture carrying it rather than the corpus.
