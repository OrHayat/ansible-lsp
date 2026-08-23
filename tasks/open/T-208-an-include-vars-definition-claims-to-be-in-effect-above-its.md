# T-208 — An include_vars definition claims to be in effect above its own include task

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P2       | M    | —          |

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

- [ ] a use above an `include_vars` does not see what it defines; a use below it does — both
      asserted in one file, since the pair is the claim
- [ ] the same asserted through hover, which is where it is visible
- [ ] a use in a *different* file from the include still sees it — the conservative arm, kept
- [ ] `vars_files`, role `vars/` and play `vars:` still apply everywhere in the file, asserted,
      so this fix cannot start ordering things that bind before the run
- [ ] `undefined_uses` is unchanged — it uses `reaches`, and a name defined by a later
      `include_vars` must still not be reported undefined
- [ ] seen red before the fix

**Turned up by:** reviewing [[T-207]], which made it reachable in the re-include case.
