# T-202 — A relative ansibleLsp.inventory resolves against folder 1, so a multi-root window answers from the wrong folder

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | bug  | P1       | M    | T-201      |


## Symptom

A VS Code window can hold more than one top-level folder (*File -> Add Folder to Workspace*,
saved as a `.code-workspace`). One server serves the whole window, and a request can arrive
for a file in any folder. `ansibleLsp.inventory` is one window-scoped setting standing in for
`-i`; when its value is **relative**, every folder's files resolve it against folder #1.

Measured (probe `t201_probe_multi_root`, `main.rs`): two folders, each with its own `inv.ini`
setting `control` to a different value, `ansibleLsp.inventory: ["inv.ini"]`, hover on
`vars/{{ control }}.yml`.

| roots | file asked about | hover says | source link |
| ----- | ---------------- | ---------- | ----------- |
| `[A, B]` | A/play.yml | `control` = `11` | A/inv.ini |
| `[A, B]` | **B/play.yml** | **`control` = `11`** | **A/inv.ini** |
| `[B, A]` | B/play.yml | `control` = `22` | B/inv.ini |
| `[B, A]` | **A/play.yml** | **`control` = `22`** | **B/inv.ini** |

Two lies in one tooltip: a value that folder B's inventory does not contain, and a
go-to-source link that opens a file in a different project. The answer depends on which folder
the user happened to add first, which nothing on screen explains.

Rows 3 and 4 are the control - the answer flips with the root order, so a fixture whose
inventory was never read at all would have shown no substitution in any row and the probe
would have failed instead of confirming.

The failure is worse than a wrong value when the relative path does not exist in folder #1:
the resolved path is missing, **no inventory is read at all**, and every inventory-derived
variable silently vanishes. That is the same silent-absence mode [[T-201]] documents.

## Cause

Since [[T-201]], `main.rs`, `State::inventory_setting`:

```rust
let root = self.roots.lock().ok().and_then(|r| r.first().cloned());
raw.into_iter()
    .map(|p| match (&root, p.is_absolute()) {
        (Some(r), false) => r.join(p),
        _ => p,
    })
```

`first()` is the whole bug: every relative configured path joins onto folder #1, and the
folder the *file* belongs to is never consulted. Before T-201 it was not even in scope - the
resolution lived in a free function over two process globals that received no path. T-201 put
the whole `roots` vector in scope at this one site without changing the rule; this ticket
changes the rule.

`initialize` already knows the setting is window-scoped and says so at `:2592`: "in a
multi-root window a folder-level settings.json is ignored for window-scoped keys." That
comment describes the input. This ticket is about the output.

## Fix

Resolve a relative `ansibleLsp.inventory` against the workspace folder **that contains the
file being asked about**, not against `roots.first()`.

The rule has to be written down before it is coded, because these cases all exist:

| case | what to do |
| ---- | ---------- |
| file under exactly one root | resolve against that root |
| file under no root (a jump into `~/.ansible/collections`, an included file outside the window) | fall back to `roots.first()`, as today |
| **file under two roots (B is a subdirectory of A - VS Code allows adding a nested folder)** | undecided; see below |
| the configured path is absolute | unchanged, no root involved |

The nested case is the one that needs a decision and a measurement, not a guess. "Longest
matching prefix wins" matches how VS Code scopes a folder-level `settings.json`, but it has a
silent failure: if `A/sub/inv.ini` does not exist, resolving there reads **no inventory**,
which is worse than today's answer of `A/inv.ini`. Candidate rule, to be pinned by a test:
resolve against every containing root, longest prefix first, and keep the first that exists -
falling back to `roots.first()` only when none does. The existing usable-path filtering
(`the_inventory_setting_keeps_only_usable_paths`, `main.rs:5365`) is the precedent for
dropping a path that cannot answer rather than passing it down.

A second question the work has to answer rather than assume: **should the status bar say
which folder answered?** `publish_inventory` (`:1197`) currently reports one inventory for the
window, computed from `roots.first()`. T-062's whole argument is that a tool which picks an
inventory silently reproduces the ambiguity it exists to solve, so a per-folder answer that is
invisible is only half a fix.

### The rule as landed

`State::inventory_setting_for(path)` (`main.rs`), with `inventory_setting()` kept as the
window-level answer for the one caller with no file in hand, the status bar. Pinned by
`a_relative_inventory_setting_resolves_against_the_folder_containing_the_file`, every row
under both root orders:

| case | what happens | why |
| ---- | ------------ | --- |
| absolute entry | as written | no folder involved |
| under exactly one root | that root | the file's project is unambiguous |
| under nested roots, entry present in the deeper | the deeper root | matches how VS Code scopes a folder-level `settings.json` |
| under nested roots, entry absent in the deeper | walk outward, first root where it exists | same project either way, so the outward step is not a lie, and it avoids reading no inventory |
| under one root, entry absent, no enclosing root | the missing path, **kept** | never a sibling's copy - that is the wrong-project answer this ticket removes. `inventory::sources` drops it, nothing is read, and the status bar names the folder it is missing from |
| under no root | `roots.first()`, as before | the request cannot say which project the user came from; a stable guess |

The nested rows are the candidate above, measured rather than assumed: the test builds
`A/sub-with/inv.ini` and `A/sub-without/` and asserts the deeper file for the first and
`A/inv.ini` for the second. The one departure from the candidate is the fallback target:
"falling back to `roots.first()` when none exists" was written for the nested case, and read
literally it would send a sibling folder's file to a project it does not belong to. The
fallback only ever walks *enclosing* roots; a first root that does not contain the file is
reached only when no root does. Containment is checked canonicalised and raw both, because a
path that does not exist cannot be canonicalised and on Windows the canonical form carries a
prefix the raw one lacks.

**The status bar answers per folder.** `inventory_status` gains a `folders` array - one entry
per root with its own `resolved` list and a `missing` list of configured relative entries it
has no file for. The window-level fields keep their first-folder meaning, so the picker panel
built on them is unchanged. The client appends a per-folder section to the tooltip when the
window has more than one root, and adds the warning glyph when any folder is missing its
entry - that folder reads nothing, and nothing else on screen would say so. The reason for
"per folder in the tooltip" rather than "the active editor's folder in the text": the server
does not know which editor is active, and the status bar item is one per window, so listing
every folder is the answer that is true whichever file has focus.

**No multi-root demo.** The demo harness (`demo_exercises_every_problem_and_verdict` and its
siblings) walks one root and cannot open a `.code-workspace`, so a `demo/multiroot/` label
would have nothing pinning it, which is what rule 4 forbids. The five tests above are the
record instead; the hover one, `each_workspace_folder_answers_from_its_own_inventory`, is
the same shape a demo would have shown by hand.

**The consumer tests**, one per site in the table below, each run under `[A, B]` and `[B, A]`
and each seen red with `containing_roots` short-circuited to empty (which restores the old
`roots.first()` rule): the hover (`each_workspace_folder_answers_from_its_own_inventory`),
the unparseable message (`each_folders_inventory_file_is_judged_an_inventory_source`), the
publish path through `unknown-host` (`unknown_host_reads_the_inventory_of_the_folder_the_play_is_in`,
via the new `State::inventory_diagnostics`, which is the publish path's inventory-dependent
half factored out so it can be reached without a `Client`), and the status bar
(`the_inventory_status_reports_each_folder_separately`). Every one failed at the folder-B
assertion with folder A's answer - the symptom, not an unrelated break.

The ignored hover test needed one edit to pass: `inventory_setting_for(&file)` in place of
`inventory_setting()`. That is not the rule changing; it is the resolver now having to be told
which file is asking, which is the fix itself.

## How to test it

**The failing test already exists.** `each_workspace_folder_answers_from_its_own_inventory`
(`main.rs`, at the end of the test module) asserts the answer this ticket owes and is
`#[ignore]`d with the reason, following the same convention as the two `role_path` tests
waiting on T-067/T-068. The scratchpad probe it replaced is deleted - a printing script and a
committed assertion are the same evidence, and only one of them can go red.

Run it with:

```
cargo test -p ansible-lsp -- tests::each_workspace_folder_answers_from_its_own_inventory --exact --ignored
```

It fails today at the folder-B assertion, showing `control` = `11` sourced to
**folder A's** `inv.ini`. The folder-A assertion above it passes, and is the control: without
it, "B is wrong" would read the same as "no inventory was read at all".

What it covers, and what it does not:

- **covered** - two sibling folders, one window-scoped relative `ansibleLsp.inventory`, each
  folder's file expected to answer from its own `inv.ini`, with the source link checked as well
  as the value. A wrong link into another project is half the harm here.
- **not covered, on purpose** - the nested case (folder B *inside* folder A). The rule for it is
  still open below, and a test asserting a guess would be worse than no test. Add it once the
  rule is decided, in the same shape.
- **not covered** - the other three consumers of `inventory_setting()` in the table below. This
  test goes through the templated-path hover only. Rule 3 wants one per consumer.

**Un-ignore it as the first step of the fix.** If it needs editing to pass, the rule changed
and this ticket should say why.

Per rule 3, a test **per consumer**, not one for the rule. `inventory_setting()` feeds four
sites, and each answers a different user-visible question:

| consumer | `main.rs` | question it answers |
| -------- | --------- | ------------------- |
| `cached_definitions` | `:198` | every variable hover, jump and undefined-use diagnostic |
| `unparseable_diagnostic` | `:482` | is this file an inventory source (which changes the message) |
| publish-diagnostics path | `:718` | `unknown-host` |
| `publish_inventory` | `:1198` | the status bar |

Each needs a two-root fixture asserting it answers from the *file's* folder. `:482` is the one
most likely to be forgotten and the one where being wrong is loudest: a file misjudged as an
inventory source gets a different diagnostic entirely.

The control that keeps every one of them honest is the one the probe already has: the same
question with the root order reversed must produce the *other* folder's answer. An assertion
that only checks "folder B's file says 22" also passes when no inventory was read and the
substitution silently did nothing.

## Demo

`demo/` is a single folder with one `ansible.cfg`, so it cannot express this - the bug needs
two top-level folders and a `.code-workspace`. Options, to be decided as part of the work:

- a `demo/multiroot/` holding `a/` and `b/` plus a committed `.code-workspace`, labelled with
  the usual **GOOD**/**BAD** prefixes, and opened by hand when checking this rule. Rule 4 says
  a demo label is a claim that needs a test pinning it, and the test would be the two-root
  fixture above rather than the demo-diagnostics harness, which walks one root.
- or: no demo file, and the ticket says why in one line. A demo nobody can open the way the
  harness opens the others is a label with nothing pinning it, which rule 4 exists to prevent.

The second is the safer default. Whichever is chosen, it goes in writing here - an absent demo
that was decided against and an absent demo that was forgotten look identical later.

## Done when

- [x] the resolution rule is written down here, including the nested-root case, **with the
      nested case measured** rather than assumed
- [x] a relative `ansibleLsp.inventory` resolves against the workspace folder containing the
      file, with `roots.first()` kept only as the no-containing-root fallback
- [x] a resolved path that does not exist falls back rather than silently reading no inventory
- [x] the harness is a committed, `#[ignore]`d test rather than a printing probe -
      `each_workspace_folder_answers_from_its_own_inventory`, seen to fail for the documented
      reason
- [x] that test is un-ignored and passes, and the nested-root case is added to it once the
      rule below is decided
- [x] a two-root test per consumer of `inventory_setting()` - all four sites in the table
      above, each with the reversed-root-order control
- [x] the status-bar question is answered: either `publish_inventory` reports per folder, or
      the ticket records why one window-level answer is still honest
- [x] the demo question is answered in writing, either way
- [x] verified red first: with the fix reverted, each new test fails for the right reason
      (rule 5), and the mutation is confirmed present in the file before concluding anything
