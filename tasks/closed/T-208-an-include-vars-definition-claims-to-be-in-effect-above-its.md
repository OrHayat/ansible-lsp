# T-208 — An include_vars definition claims to be in effect above its own include task

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | bug  | P2       | M    | —          |

## Symptom

`include_vars` loads at a point in the run. A use *above* that task cannot see what it
defines — but we say it can.

Measured by hovering the same name twice in one role task file, once either side of the
include:

```yaml
- debug: {msg: "before {{ thing }}"}   # only role vars (15) is in effect here
- include_vars: main.yml
- debug: {msg: "after {{ thing }}"}    # now include_vars (18) is
```

Both hovers render `include_vars · ad/vars/main.yml:1 = X  ← effective`. The second is
right; the first names a level that has not been loaded yet at that point in the play.

Where it shows: hover and go-to-definition, which are the two consumers that ask "what does
this read *here*" — `in_effect_for` (`main.rs:1853,1974`). `undefined_uses` is unaffected: it
filters on `reaches`, which is scope-only and deliberately ignores run order (rule 3's
corollary, T-100).

## Cause

`ordered_before` (`vars.rs:619`) orders only `set_fact` and `register`:

```rust
VarSource::SetFact | VarSource::Register => self.file != use_file || self.span.start < use_pos,
_ => true,
```

`IncludeVars` falls to `_ => true`.

**Adding it to that arm would do nothing**, which is the part worth knowing before anyone
tries. For `set_fact` the `Located`'s `file`/`span` are the *task* that sets it, so comparing
`span.start` against the use works. For `include_vars` they are the **loaded vars file** — the
hover above reads `ad/vars/main.yml:1`, a position in a different file from the use. So
`self.file != use_file` is true and the guard returns "in effect" regardless of position.

The include task's own position is not carried on `Located` at all. That is the actual gap.

Pre-existing, and not caused by [[T-207]]. What T-207 changed is the visibility: before it,
the re-include collision collapsed to a single `RoleVars` entry, so there was no `IncludeVars`
def to mis-order in that case. Now both survive and the higher one wins `effective()`
everywhere in the file, including above the include. The trade T-207 made, measured:

| use position | before T-207 | after T-207 |
| --------------------- | ------------------ | ------------------------------- |
| above the include | `role var` — right | `include_vars` — **wrong** |
| below the include | `role var` — wrong | `include_vars` — right |

Better on balance, since a variable is normally read after the include that loads it, but the
top half is a regression and is why this is filed rather than left.

## Fix

Carry the include site — the task's file and byte offset — on definitions sourced from
`include_vars`, and order against *that* rather than against the position in the loaded file.
`Located` already carries `via` for a route that isn't visible in the file being read; this is
the same shape of fact and may belong there rather than in a new field.

Cross-file stays conservative, as `ordered_before` already is: if the include task is in a file
other than the use's, we cannot order the two and must keep the definition rather than guess.

Check `vars_files` and role `vars/` while here — both bind before tasks run, so they are
correctly unordered today, and a change to this function must not start ordering them.

## Done when

- [x] a use above an `include_vars` does not see what it defines; a use below it does — both
      asserted in one file, since the pair is the claim
      (`an_include_vars_definition_reaches_uses_below_it_and_not_above`)
- [x] the same asserted through hover, which is where it is visible — above shows one level and
      never says `include_vars`; below shows both with the include leading
- [x] a use in a *different* file from the include still sees it — the conservative arm, kept
- [x] `vars_files`, role `vars/` and play `vars:` still apply everywhere in the file, asserted at
      offset 0, so this fix cannot start ordering things that bind before the run
- [x] `undefined_uses` is unchanged — a name used above its `include_vars` is still not reported
      undefined, because that filter runs on `reaches` and never on run order
- [x] seen red before the fix — the two ordering tests failed with the rule removed; all three
      controls stayed green, which is what a control is for

## Review findings, after it closed

- **Each include is ordered against its own task**, including one nested in a `block:` —
  three includes in one file get three strictly-increasing sites. Pinned by
  `each_include_vars_is_ordered_against_its_own_task`, which fails when every site is
  collapsed to one; the two-task test alone does not catch that.
- **A dead guard, and a comment justifying an impossible case.** `stamp_include_site` skipped
  definitions that already carried a site, with a comment explaining when that would matter.
  It never fires: [`read_var_file`] walks key/value mappings and never recurses into tasks, so
  nothing in the range can be pre-stamped. Verified by asserting over the whole suite and the
  demo tree, then replaced with an unconditional write and the real reason.
- **Memo-safe**: the site is a property of the file being collected, so a memoized
  contribution carries the right one no matter which parent reached it.
- **`Located` derives only `Debug, Clone`** — no `PartialEq`, so the new field cannot change
  an equality comparison anywhere.
- **The dir form was untested.** Every ordering test used the file form, and the two take
  different branches — the dir form runs the ported plugin walk over a list of files. It is
  correctly stamped and ordered, but nothing said so until
  `the_dir_form_is_ordered_against_its_task_too`.
- Demo tree unchanged: 670 defs, identical diagnostic counts.
- **Wall clock unchanged.** `var_walk` against the same demo tree, baseline `ea29004` vs
  current: 9.27/10.03/10.00 ms against 9.59/9.99/9.80 ms — overlapping, no regression from the
  new field or its `PathBuf` clone per include-sourced def.

  The first attempt at this compared a worktree running its *own* demo tree against the
  current one, so the input differed from the code and the number meant nothing; the second
  ran both loops in the same directory and could not have shown a difference at all. Rule 2
  applies to the commands you verify with. Recorded because "measured, no regression" is only
  worth anything if the measurement could have said otherwise — and this one is at demo scale
  (76 files, 670 defs), so it says nothing about a large tree with heavy dir-form includes.

## What landed

`Located::after` — the (file, offset) of the task that loads the definition, set for
`include_vars` only and stamped the way `via` already is, on whatever the read added.
`ordered_before` checks it first and keeps the same across-files rule the `set_fact` arm uses.

The field exists because the obvious fix could not work: an `include_vars` definition's own
`file`/`span` are inside the **loaded vars file**, so there was no position to order against.
That is written on the field rather than only here, since it is the reason the field is not
redundant with `span`.

**Turned up by:** reviewing [[T-207]], which made it reachable in the re-include case.
