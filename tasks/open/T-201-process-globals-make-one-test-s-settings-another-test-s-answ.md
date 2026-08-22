# T-201 — Process globals make one test's settings another test's answer

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P1       | L    | —          |

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

### Measured: a multi-root workspace does get a wrong answer

Box 1 is answered, and the answer is yes. Probe: two workspace folders, each with its own
`inv.ini` defining `control` to a different value, a relative `ansibleLsp.inventory:
["inv.ini"]`, and the templated-path hover asked about `vars/{{ control }}.yml` in each
folder.

| `WORKSPACE_ROOT` | file asked about | hover says | sourced to |
| ---------------- | ---------------- | ---------- | ---------- |
| folder A | A/play.yml | `control` = `11` | A/inv.ini |
| folder A | **B/play.yml** | **`control` = `11`** | **A/inv.ini** |
| folder B | B/play.yml | `control` = `22` | B/inv.ini |
| folder B | **A/play.yml** | **`control` = `22`** | **B/inv.ini** |

Row 2 is a wrong value *and* a clickable link into the wrong folder. Rows 3 and 4 are the
control: the answer flips with the root, so the probe could have come back unchanged — a
fixture whose inventory was never read at all would have shown no substitution in any row —
and it did not.

`WORKSPACE_ROOT` is `roots.first()` (`main.rs:2588`), so in a window with more than one
folder every relative inventory path resolves against folder #1 for files in *all* of them.

**This raises the priority to P1** — the board's P1 is "the tool lies", and this is a wrong
value with a wrong source link, not a flake.

**The wrong answer is split out to [[T-202]].** Removing the globals does not fix it: threading
`roots.first()` cleanly to every caller gives folder B folder A's inventory just as wrongly.
That needs a *resolution rule* — which root a relative path belongs to — which is a design
decision with its own tests and demo. T-201 stays what it is: the globals go, so that a
per-file answer has something to travel in. T-202 depends on it.


### A reproducer that does not need luck

Building the multi-root probe produced a better handle on this bug than the sleep-widened
window. The probe (`scratchpad/t201_multiroot_probe.rs`) writes `INVENTORY_SETTING` and
`WORKSPACE_ROOT` to set up its two folders. Measured, `cargo test -p ansible-lsp`, 10 runs
each:

| probe | victim fails | mechanism |
| ----- | ------------ | --------- |
| absent | **0/10** | - |
| present, restores the globals at the end | **6/10** | the race, on a window widened by the probe's own I/O - matches the 7/10 the `sleep(300ms)` experiment gave |
| present, does **not** restore | **10/10** | not a race at all: the globals stay set for every test that runs afterwards |
| probe + victim alone, probe not restoring | **10/10** | same |

The two mechanisms are worth keeping apart. The 6/10 row is the bug in the Symptom - a
window one writer opens and another reads through. The 10/10 rows are a *leak*: a test that
never restores what it wrote poisons the rest of the process permanently. Same hazard, and
only the second is deterministic.

The 10/10 pair is therefore the reproducer to use while fixing this: it needs no repetition
and no timing, and after the fix the probe's writes cannot reach the victim at all because
there will be nothing process-wide to write. It is also why the probe is **not** committed to
the suite - a test that writes a global is the thing this ticket exists to remove, so it stays
in `scratchpad/` until [[T-202]] can promote it against a fixed design.


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

- ~~**Does a multi-root workspace get a wrong answer today?**~~ Measured above: **yes**.
  Priority raised to P1, and the wrong answer itself is [[T-202]].
- **Does the var cache follow?** It is global for a reason - a cross-file index shared by every
  request. Removing it is a different, larger change, and it may be right to leave it and say
  why at the site.
- **Or serialize instead?** A test-only mutex around the writer would stop the flake for a
  fraction of the cost and change nothing about the design. That is the cheap option and it
  should be priced honestly against the real one, not dismissed - but it leaves the hazard,
  and the hazard is what this ticket is about. There is a stronger precedent than a mutex in
  this repo: `crates/ansible-core/tests/process_env_snapshot.rs` isolates a process-mutating
  test by giving it its own **integration binary** - cargo runs each one in its own process -
  with the reasoning written at the top of the file.

## Every global in the process

Enumerated by grepping `\bstatic\b` across `ansible-lsp` and `ansible-core` - `src/`,
`tests/` and `examples/`, lifetimes filtered out. Seven, and no others; there is no
`set_var` or `set_current_dir` anywhere in `src/`, so the environment is read-only there.
Statics inside dependencies (tokio, tower-lsp, regex) are not ours and are out of scope.

| # | Global | Site | Written by | Can one caller change another's answer? |
| - | ------ | ---- | ---------- | --------------------------------------- |
| 1 | `INVENTORY_SETTING` | `main.rs:224` | `set_inventory`, from `initialize` `:2600`, `did_change_configuration` `:2658`, 2 tests | **Yes** - it decides which inventory is read. This is the flake and [[T-202]] both |
| 2 | `WORKSPACE_ROOT` | `main.rs:222` | `initialize` `:2587` only | Not in tests (they build `State` directly and never call `initialize`, so it stays `None`); in production it is the multi-root wrong answer |
| 3 | `VarCache` `OnceLock` | `main.rs:184` | `set_inventory` `:107`, `invalidate_var_cache` `:339` | Staleness only - keyed by canonical path, so a bad read is a recompute |
| 4 | `install::DETECTED` | `install.rs:195` | `detect()` `get_or_init`, 13 call sites | Process-scoped by nature - the installed Ansible does not vary per request |
| 5 | `install::OVERRIDE` | `install.rs:198` | `set_package_dir_override`, from `initialize` `:2608` | `OnceLock::set` - a second write is **silently dropped**, so its doc's "per-project and live on reload" is a claim the type forbids |
| 6 | `module_redirect::TABLES` | `install.rs:406` | memoised parse, keyed by path | Pure cache |
| 7 | `splitter::ESCAPES` | `splitter.rs:205` | `LazyLock<Regex>`, one reader (`decode_escapes` `:216`) | No - a compiled constant with no writer |

Note that 1 and 2 are **shadow copies of fields `State` already has** - `State::inventory`
(`:377`) and `State::roots` (`:364`). They exist only because `inventory_setting()` is a free
function on a path that holds no `State`, which is precisely the situation [[T-199]] solved
for open buffers.

## What doing (1) turned up: the cache key was hiding the same bug

The de-globalising is mechanical - `inventory_setting()` became a method on `State`, which
already owned both halves, and the value is passed to the six readers alongside the `OpenDocs`
that [[T-199]] threads on the same route. The part that was not mechanical:

**The var cache was keyed by path alone.** With the setting global there was only ever one
inventory in flight and `set_inventory` cleared the cache wholesale on change, so a path-only
key was safe by accident. The moment the value travels with the request, two requests can
legitimately carry different inventories - and the second one was handed the first one's
answer. The first version of `each_request_answers_from_the_inventory_it_was_given` failed on
exactly this: request B asked with `inv_b.ini` and got `control` = `11` sourced to
`inv_a.ini`.

Fixed by keying entries on `(canon(path), inventory_key(inv))`. Two consequences worth having
in writing:

- `invalidate_var_cache` now drops **every** inventory's entry for a file, not one, since
  "the entry for this file" became a set.
- `analyze_text_measured` (`:664`) builds its own `ScanCache` and never called
  `with_inventory`, so it passes `&[]` and now keys separately from the hover path. Under the
  old path-only key those two shared an entry, which means **whichever ran first decided
  whether the setting applied at all** - a latent inconsistency of the rule-3 kind that was
  invisible until the key made the two answers distinguishable. Behaviour on each path is
  unchanged; they simply no longer overwrite each other.

### Verification

- `cargo test --workspace`: all nine targets green (`ansible-lsp` 95 -> 98).
- `cargo test -p ansible-lsp` **30 runs, 0 failures**. The victim,
  `group_priority_substitutes_a_path_from_a_host_position_and_never_from_a_group`, no longer
  has a slot for another test to write.
- Rule 5, three breaks, each confirmed present in the file before drawing a conclusion:

  | break | expected | got |
  | ----- | -------- | --- |
  | `cached_definitions` ignores the `inv` it was handed | the two hover tests fail, the value test does not | 2 failed, 1 passed |
  | the cache key stops distinguishing inventories | only the two-request test fails | 1 failed, 2 passed |
  | `inventory_setting` ignores the `State` it was called on | all three fail | 3 failed |

- The multi-root bug is confirmed **unchanged** ([[T-202]] re-run after the fix: folder B's
  file still answers `11` from folder A's `inv.ini`). That is the intended outcome - this was
  a de-globalising change, not a behaviour change.

One thing went wrong in the writing and is worth recording, because it is this ticket's own
bug in miniature: the three new tests first shared one `testing::project` name. `tree()` says
in its doc that a name must be unique per test or they race, and they did - one of them failed
only when run alongside the others, and passed alone.


## What (1)-(3) cost, and what they turned up

### The cache key was hiding the same bug

`inventory_setting()` became a method on `State`, which already owned both halves, and the
value is passed to the six readers alongside the `OpenDocs` [[T-199]] threads on the same
route. That part was mechanical. This part was not:

**The var cache was keyed by path alone.** With the setting global there was only ever one
inventory in flight and `set_inventory` cleared the cache wholesale on change, so a path-only
key was safe by accident. The moment the value travels with the request, two requests can
legitimately carry different inventories - and the second was handed the first's answer. The
first version of `each_request_answers_from_the_inventory_it_was_given` failed on exactly
that. Fixed by keying on `(canon(path), inventory_key(inv))`.

Two consequences in writing:

- `invalidate_var_cache` drops **every** inventory's entry for a file, since "the entry for
  this file" became a set.
- `analyze_text_measured` (`:664`) builds its own `ScanCache` and never called
  `with_inventory`, so it passes `&[]` and keys separately from the hover path. Under the old
  key those two **shared an entry, so whichever ran first decided whether the setting applied
  at all** - a latent rule-3 inconsistency, invisible until the key made the two answers
  distinguishable. Each path's behaviour is unchanged; they no longer overwrite each other.

### The handler layer had no coverage at all, and seven wiring lines survived mutation

Every test in `main.rs` entered *below* the handler - `set_inventory`, `hover_at`,
`analyze_text` - which proves the machinery works and says nothing about whether the server
ever calls it. Measured by deleting one line at a time and running the suite:

| line deleted | before | after |
| ------------ | ------ | ----- |
| `did_open`/`did_change`/`did_close` don't invalidate | survived | caught |
| `did_change` never stores the new text | caught | caught |
| `initialize` records no workspace roots | survived | caught |
| `initialize` drops `initializationOptions` inventory | survived | caught |
| `did_change_configuration` ignores the new settings | survived | caught |
| `did_change_configuration` doesn't re-publish the inventory | survived | caught |
| `publish_diagnostics` stops tracking what it published | survived | caught |
| `publish_diagnostics` never sends | survived | caught |
| `did_close` leaves the buffer in the map | survived | caught |
| `initialized` never starts the scan | survived | caught |
| **`initialize` ignores `ansiblePath`** | survived | **still survives - this is box (5)** |

The last one cannot be tested until `install::OVERRIDE` stops being a `pub(crate)` `OnceLock`
with no getter. Nothing can read back what `initialize` set, and being write-once, a second
test in the same process could not set it anyway. **Box (5) is what makes that test writable**,
which is the reverse of the order this ticket assumed.

### What made the handler layer reachable

`Client` has no public constructor - the closure passed to `LspService::new` is the only
source - so nothing could reach `scan_workspace`, `publish_inventory` or any `did_*` handler.
The harness is 25 lines and needed no transport, no duplex pipe and no JSON-RPC framing; the
pattern is the one tower-lsp uses in its own `service.rs` tests, which was in the vendored
source the whole time. Two facts it cost real time to learn, both recorded at the call site:

- The socket is **dropped**, not held. `Client`'s send is
  `if tx.send(req).await.is_err() { return Err(...) }` (`service/client.rs:549`), so with the
  receiver gone every publish fails instantly and is swallowed. *Holding* it unpolled is what
  deadlocks - the channel is `mpsc::channel(1)`.
- For the two tests that read what was sent, the socket is held and drained, and the drain
  terminates on `drop(service)` rather than a timeout. Those tests first collected only
  `["window/logMessage", "window/logMessage"]`: **`Client::send_notification` suppresses every
  notification until the server is initialized** (`service/client.rs:441`), and calling
  `Backend::initialize` directly does not set that - only routing a real `initialize` request
  through the service layer does (`service/layers.rs:73`). Read as "our code sends nothing",
  that would have been a bug report against code that is correct.

### Two production changes that stand on their own

Both are the shape of `unparseable_diagnostic`/`_for` and `diagnostics_of`/`_with`, not a
test-only seam:

- `publish_inventory` split into `inventory_status` (all the decisions, returns the payload)
  and delivery. The status bar's claims - the setting beats `ansible.cfg`, *which* cfg was
  read, what a plain `ansible-playbook` would have read - are now assertable as a value.
- `initialized` keeps the scan's `JoinHandle` in `State::scan_task` instead of discarding it.
  A bare `tokio::spawn` swallows the task's panic, so a scan that dies takes every diagnostic
  with it silently; `shutdown` also now has somewhere to `abort()` from. Both a deleted spawn
  and a *discarded handle* fail the test.

### Verification

- `cargo test --workspace`: nine targets green, `ansible-lsp` 95 -> 107.
- `cargo test -p ansible-lsp` **30 runs, 0 failures** (2/30 before).
- Zero compiler warnings, used as a check in its own right: an ignored `cache` parameter warns,
  which is how three helpers were caught taking the parameter and still calling `no_cache()`.
  Probed that the check can fail before trusting it.
- Rule 5, every break confirmed present in the file first: `cached_definitions` ignoring its
  `inv` (2 failed), the cache key ignoring the inventory (1), invalidation dropping nothing
  (2), `inventory_setting` ignoring its `State` (3), the scan getting its own cache (1), the
  status precedence rule inverted (1), plus the eleven handler mutations tabled above.
- The multi-root bug is confirmed **unchanged** ([[T-202]]) - this was a de-globalising change,
  not a behaviour change.

Two things went wrong while writing this and both are worth keeping:

- The three new T-201 tests first shared one `testing::project` name. `tree()`'s doc says a
  name must be unique per test or they race - and one failed only alongside the others.
- A helper-fixing script exited before its `write()`, so `t201_hover`, `t199_hover` and
  `t199_definition` took a `cache` parameter and ignored it. Every cache test was passing
  vacuously, and the only reason it surfaced is that **rule 5's breaks stopped biting**. A
  green suite proved nothing; the mutations did.


## Box (5), and the rule the code was already breaking

`OVERRIDE` could not be removed on its own terms: its only job was seeding `from_filesystem`,
which ran inside `DETECTED.get_or_init(...)`. Passing the override to `detect()` instead would
have kept "whoever calls first decides, silently" - the same bug in a different spelling. What
made it separable from (4) was noticing that `detect()` should not have existed at all.

`detected()`'s own doc says:

> Anything on a request path must use this: `detect` is `get_or_init`, so the first caller pays
> the whole cost, and on the message pump that is the 3.6 s freeze T-084 measured.

**Four callers were breaking that rule**: `resolve_module` (`resolve.rs:629`, `:636`, `:656`),
which every hover, jump and diagnostic goes through, and `FileContext::collection_roots`
(`workspace.rs:185`). Masked because `startup` normally wins the race - a request arriving
first paid the 3.6 s on the pump.

So the shape is:

```rust
pub fn init(override_dir: Option<PathBuf>) -> &'static Self   // one caller: startup
pub fn detected() -> Option<&'static Self>                    // everyone else
```

`detect()` is gone, so no request path can start detection by accident, and the four callers
above now answer without the install rather than blocking. `DETECTED` stays - that is (4) -
but it is written from exactly one place instead of by whoever gets there first.

### What is pinned, and the half that is not

`initialize_records_the_ansible_path_setting` and `a_blank_ansible_path_is_no_setting_at_all`
cover the recording; `an_explicit_package_dir_beats_detection_and_is_labelled_as_the_override`
(in `install.rs`) covers detection honouring it, asserted against `run` rather than `init` so
no test writes the process-wide `DETECTED`. Its control is a directory with no `modules/`,
which must *not* come back as `Source::Override`.

The handoff itself - `let ansible_path = state.ansible_path();` in `startup` - is **not**
pinned. Replacing it with `None` leaves the suite green. Observing it means observing what
`init` received, and that needs detection to stop being a process singleton: **box (4)**.

### A dormant test, found by a compiler warning

Chasing an unrelated warning in `ansible-core` turned up this in `8bd16ed` (2026-08-20):

```
3010  #[test]                                  <- T-062's attribute
3011  /// The wiring half of T-178...          <- a different test inserted between
3019  #[test]
3020  fn group_priority_reaches_the_index...   <- received both
3056  fn a_configured_inventory_reaches_..._does_not()   <- received none
```

The T-178 test landed *inside* the T-062 test's header, so T-062's `#[test]` bound to the
wrong function and `a_configured_inventory_reaches_the_index_and_a_dynamic_one_does_not`
stopped being a test for two days. Verified by `git show` of that commit and its parent, not
inferred from a blame date. It was never `#[ignore]`d - it compiled as an ordinary unused
function, which is why the only symptom was a `dead_code` warning.

Its own doc records what it is for: *"the parsers passing proves nothing about the wiring -
deleting the call site left every other test in this file green."* A test written to catch a
silently deleted call site was itself silently deleted.

Reattached, and proven able to fail: emptying the inventory-sources loop (`vars.rs:1220`)
takes it red with two others. It passes on current code, so nothing was hiding behind it.

**The lesson for this ticket:** zero compiler warnings is worth treating as a check, not
cosmetics. `dead_code` on a test function means the test is not running, and nothing else in
the workflow says so - the suite reports a smaller number and looks green.


## Done when

One box per global. The bar is the same for each: the value either travels with the caller,
or it is proven to hold no request state and the proof is written at the site.

- [x] the multi-root question is measured and the answer recorded here, with the priority
      adjusted to match - **done: yes, wrong answer; P1; the fix is [[T-202]]**
- [x] **(1)** `INVENTORY_SETTING` is gone, and all four readers (`:198` `cached_definitions`,
      `:482`, `:718`, `:1198`) take the value from their caller - **done**, see below
- [x] **(2)** `WORKSPACE_ROOT` is gone - **done**. It fell out with (1): `inventory_setting`
      is now a method on `State`, which already holds `roots`. The resolution rule is
      deliberately still `roots.first()`; [[T-202]] owns changing it and now has the whole
      `Vec` in scope at the one place that reads it
- [x] **(3)** the `VarCache` `OnceLock` is gone - **done**. It is a `State` field, still
      reached by every reader through the `Arc` (the detached scan included), and its key now
      carries the inventory. `ansible-lsp` has **no process globals left**
- [ ] **(4)** `install::DETECTED` is gone - the detected install is a value owned by `State`
      and passed to its 13 call sites across `resolve.rs`, `workspace.rs` and `main.rs`
- [x] **(5)** `install::OVERRIDE` is gone - **done**, and *without* (4). It became an argument
      to `AnsibleInstall::init`, and the path it carries now lives on `State::ansible_path`.
      The reload is settled the second way the box allowed: a changed `ansiblePath` needs a
      restart, said in the doc rather than dropped in silence by `OnceLock::set`
- [ ] **(6)** `module_redirect::TABLES` is gone - it moves onto `ScanCache`, which is already
      the per-request memo (`cache.rs`, `Map` fields) for exactly this kind of parse
- [ ] **(7)** `splitter::ESCAPES` is gone - see the open question below before doing this one
- [x] `cargo test -p ansible-lsp` run 30 times with zero failures, having first been seen to
      fail on the pre-fix binary - **0/30**, against 2/30 before
- [ ] the widened-window probe from the Symptom is re-run and now passes, since that is the
      version of the race that reproduces reliably
- [x] a test per consumer of each moved value (rule 3), not one test per global - done for
      (1), (2) and (3); see the coverage section below
- [x] the 10/10 pair reproducer is re-run after the fix and the victim passes - and the
      probe's writes are unreachable by construction: there is no process-wide slot to write
- [ ] `scratchpad/t201_multiroot_probe.rs` is promoted into the suite by [[T-202]], or the
      ticket records why it stays a scratchpad probe - an uncommitted probe with no assertion
      is a test that cannot fail

### Open question on (7), to settle before doing it

`ESCAPES` is a `LazyLock<Regex>` with no writer and one reader. It cannot carry state between
requests, so eliminating it buys no isolation; it costs a regex compile on every
`decode_escapes` call, which runs per double-quoted inventory value. Two honest ways to close
box (7): compile it once into a value threaded to the reader (real work, no benefit), or
record at the site that it is a constant and why that is not the hazard this ticket is about.
The second is probably right - but it is a decision, not an omission, so it goes in writing.
The same argument is available for (4) and (6) and is *not* as strong there: both are seeded
from configuration, and (5) is already demonstrably lying about reload.
