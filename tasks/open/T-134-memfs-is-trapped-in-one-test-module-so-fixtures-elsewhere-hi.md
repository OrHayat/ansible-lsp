# T-134 — MemFs is trapped in one test module, so fixtures elsewhere hit real disk

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P3       | M    | —          |

## Problem

The crate has an in-memory `Fs` — `MemFs` (`include_vars.rs:413-460`), a `BTreeMap<PathBuf,
String>` where a key is a file, a proper prefix of a key is a directory, and `canonical` is
the identity. `include_vars`'s ~30 tests declare a whole tree as string literals and never
touch disk.

It is `struct MemFs` inside that file's `#[cfg(test)]` block, so nothing else can reach it.
Everywhere else builds a real tree instead: `std::env::temp_dir()` appears 20 times across
`resolve.rs` (5), `vars.rs` (12), `mutation.rs`, `ansible-lsp/src/main.rs` and `board`. The
`resolve.rs` ones share a `t016_dir(name)` helper that `remove_dir_all`s a fixed path and
recreates it — a name collision away from two tests deleting each other's tree, and it does
leave the tree behind after the run.

Nothing is broken today; the tempdir tests do run and do assert. This is about what the seam
already bought and isn't collecting: `fs.rs` exists precisely so "how much work did a pass
do?" has one answer, and the same seam makes a test tree a string literal instead of a
directory. `t016_dir` is also mis-named — it belongs to T-016 and now serves T-091, T-092 and
the `vars_files` tests.

## Approach

Promote `MemFs` to a shared test helper — `pub struct` under `#[cfg(test)]` in `fs.rs`, or a
`test-fixtures` feature if the integration tests in `tests/` want it too (`CfgFs` and `NoFile`
in `network_group_modules_env.rs` and the `CfgFs` in `config.rs:125` are the same idea written
a third and fourth time). It needs one addition before it can carry `resolve.rs`: directories
that hold no files, since a role with an empty `tasks/` is a real case and the prefix rule
can't see it. An explicit dir-marker entry, or a `dirs` set alongside the map.

Then migrate the tempdir tests. `resolve.rs`'s go through `resolve_src`, which hardcodes
`FileContext::discover` and `resolve` — both have `_with`/`_in` variants taking `&dyn Fs`
(`discover_with`, `resolve_in`), so the helper changes and the test bodies mostly don't. The
`vars.rs` twelve are the bigger half and can follow separately; splitting the migration per
file is fine, the helper is the deliverable.

**Sequence this against T-077.** That ticket rebuilds every `repo()`/`$HOME`-guarded test as
an inline fixture tree. If it lands first as tempdirs, this ticket rewrites its output. Land
the helper first, or have T-077 write `MemFs` trees directly — either is fine, doing them in
the wrong order is not. (T-077's fixtures also have to use the anonymized names.)

What stays on real disk, deliberately: `no_filesystem_call_bypasses_the_seam` (`fs.rs:371`)
reads the crate's own sources, `demo_*` tests assert the checked-in `demo/` tree keeps its
promises, and anything exercising `StdFs` itself — symlinks, `read_dir` kinds — has nothing to
fake against.

## Done when

- [ ] one `MemFs`, reachable from every test module, with empty directories expressible
- [ ] `resolve.rs`'s fixture tests build no directories, and `t016_dir` is gone
- [ ] `CfgFs`/`NoFile` are the shared helper rather than per-file re-implementations
- [ ] T-077's ordering is settled in writing, so its fixtures aren't built twice
