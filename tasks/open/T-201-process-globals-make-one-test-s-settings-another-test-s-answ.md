# T-201 — Process globals make one test's settings another test's answer

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P2       | M    | —          |

## Symptom

`group_priority_substitutes_a_path_from_a_host_position_and_never_from_a_group`
(`main.rs:5986`) fails intermittently under `cargo test --workspace`, at its **control**
assertion — the one whose comment says it exists so the test "fails loudly instead of passing
empty". Measured, same binary, no source change between runs:

| how it was run | failures |
| -------------- | -------- |
| that test plus the writer below, 25 runs | 0 |
| the whole `ansible-lsp` suite, 30 runs | **2** |
| the same, with the writer's window widened by `sleep(300ms)`, 10 runs | **7** |

2/30 -> 7/10 on widening the window is the confirmation; the probe could have come back
unchanged, and that would have killed the hypothesis.

The test is not at fault — it is doing exactly what it was built to do. Its own doc comment
already predicted this failure: "an absence also appears when the inventory was never read at
all ... If that happens the control path stops substituting and this test fails loudly." It
fails at the control, never at the rule under test.

Not yet shown to be user-visible: the server writes `INVENTORY_SETTING` once from
`did_change_configuration`, so a single-workspace editor session has no second writer. What is
shown is that a **request-scoped setting lives in process-global state**, which is the same
shape as [[T-199]] — a value that should travel with the caller instead being read from a slot
anyone can write. Whether a multi-root workspace already gets the wrong answer from this is
the first thing the work should measure, and the ticket's priority should follow that answer.

## Cause

Two tests in one process, and a global that one of them writes:

```rust
// main.rs:5411, the_inventory_setting_keeps_only_usable_paths
state.set_inventory(&json!({"inventory": ["x.ini"]}));   // INVENTORY_SETTING := ["x.ini"]
...
state.set_inventory(&json!({}));                          // cleared a few lines later
```

Anything running in that window reads it, because `cached_definitions` and three other sites
build their cache from the global rather than from anything the caller passed:

```rust
ScanCache::new(...).with_inventory(inventory_setting())
```

and `with_inventory` **replaces** rather than merges (`cache.rs:294`):

```rust
pub fn with_inventory(mut self, paths: Vec<PathBuf>) -> Self {
    self.inventory_override = paths;
    self
}
```

So a fixture whose `ansible.cfg` says `inventory = inv.ini` silently gets `x.ini`, which does
not exist, and **no inventory is read at all**. Every inventory-derived variable vanishes and
the control stops substituting.

The globals in `main.rs`:

| global | written by | read by |
| ------ | ---------- | ------- |
| `INVENTORY_SETTING` (`main.rs:224`) | `set_inventory`, from settings | `inventory_setting()`, at 4 sites |
| `WORKSPACE_ROOT` (`main.rs:222`) | initialize | `inventory_setting()`, to resolve relative paths |
| the `VarCache` `OnceLock` (`main.rs:184`) | `invalidate_var_cache`, 5 sites | `var_cache()`, 7 sites |

The var cache is the least of the three — it is keyed by canonical path and a stale read is a
recompute, not a wrong answer. The two settings slots are the ones that change what an answer
*is*.

## Fix

Eliminate the globals: carry the settings the way [[T-199]] carried the open buffers — a small
value, built once per request from the state that owns it, passed to the readers.

[[T-199]] is the precedent to copy and the reason to think it is affordable: `OpenDocs` is a
path-keyed snapshot derived per request from `State.docs`, with a `Default` that means
something ("no editor behind this"), and it threaded through seven consumers without putting
the server's lifecycle object in scope of a rendering function. The same shape fits here —
inventory paths and workspace root are exactly the kind of value that should arrive with the
caller.

Open questions the work has to settle rather than assume:

- **Does a multi-root workspace get a wrong answer today?** Measure it first. If yes this is a
  user-visible bug and the priority rises; if no, this is isolation work that also removes a
  documented hazard.
- **Does the var cache follow?** It is global for a reason — a cross-file index shared by every
  request. Removing it is a different, larger change, and it may be right to leave it and say
  why at the site.
- **Or serialize instead?** A test-only mutex around the writer would stop the flake for a
  fraction of the cost and change nothing about the design. That is the cheap option and it
  should be priced honestly against the real one, not dismissed — but it leaves the hazard,
  and the hazard is what this ticket is about.

## Done when

- [ ] the multi-root question is measured and the answer recorded here, with the priority
      adjusted to match — this decides whether the rest is a bug fix or a cleanup
- [ ] `inventory_setting()` and `WORKSPACE_ROOT` are gone as globals, and every one of the
      sites in the table above takes the value from its caller
- [ ] `cargo test -p ansible-lsp` run 30 times with zero failures, having first been seen to
      fail on the pre-fix binary — the flake is the measurement, so the count is the evidence
- [ ] the widened-window probe from the Symptom is re-run and now passes, since that is the
      version of the race that reproduces reliably
- [ ] a test per consumer of the moved value (rule 3), not one test for the change
- [ ] whatever is decided about the var cache is written down at the site, not just done
