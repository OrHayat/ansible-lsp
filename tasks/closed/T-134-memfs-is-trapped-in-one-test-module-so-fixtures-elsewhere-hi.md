# T-134 — MemFs is trapped in one test module, so fixtures elsewhere hit real disk

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | task | P3       | M    | —          |

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

## Progress

`crates/ansible-core/src/testing.rs` is the shared module. `MemFs` moved there from
`include_vars.rs` and grew `with_dirs`, so a directory holding no files — a role with an empty
`tasks/` — is expressible; the prefix rule alone cannot see one. `walk` seeds those explicit
dirs before walking the files. `include_vars`'s ~30 tests run on it unchanged.

`CfgFs` is now one implementation instead of three (`config.rs`, `network_group_modules_env.rs`,
and `duplicate_dict_key_env.rs` added during T-102). `NoFile` is gone — it was `CfgFs::none()`
all along, and that case matters: an env override has to apply with no `ansible.cfg` present.

Integration tests in `tests/` compile against the crate as a dependency, so `#[cfg(test)]`
alone doesn't reach them. The module is gated `#[cfg(any(test, feature = "test-fixtures"))]`
with a self dev-dependency turning the feature on for that build — nothing test-only reaches
a release build.

`testing.rs` needed an entry in `fs.rs`'s `EXEMPT` list: `no_filesystem_call_bypasses_the_seam`
skips each file's content after an in-body `#[cfg(test)]`, and this file's `cfg` sits on the
`mod` in `lib.rs`, so it read as production code.

**`resolve.rs` is migrated.** All 16 `t016_dir` callers plus four more that built temp dirs
inline are now `MemFs` trees; `t016_dir` itself is deleted. The helper is `mem_src(file, src,
fs)` — `resolve_src`'s shape, routed through `discover_with` and the crate's `resolve_in`. It
spells out `super::resolve_in` because the test module has a *local* `resolve_in` shadowing
the crate function.

`with_dirs` proved necessary on the first real use: `include_vars_dir_targets_the_loaded_files_not_the_directory`
asserts that an empty `vars/empty` resolves with no targets, and the prefix rule can't see a
directory holding no files.

One test can't use a bare `/p` root. `vars_files_absolute_entry_is_one_candidate` turns on
`Path::is_absolute`, which is platform-defined — on Windows a rooted path with no drive prefix
is *relative*, so `/p/abs.yml` took the `vars/` prepend branch and produced two candidates
instead of one. It uses `if cfg!(windows) { "C:/p" } else { "/p" }`. Any future MemFs tree that
means "absolute" needs the same.

**Ordering against T-077, settled:** the helper landed first, as this ticket asked — but
T-077's first six conversions had already shipped as tempdir trees (`c6bcc3a`), so those six
are the ones this ticket predicted would need rewriting. T-077's *remaining* ~25 should be
written as `MemFs` trees directly and must not use `testing::tree`. `tree`/`project` stay for
the paths that hardcode `StdFs` — `FileContext::discover` has an `_with` variant, but not
every caller does.

## Done when

- [x] one `MemFs`, reachable from every test module, with empty directories expressible
- [x] `resolve.rs`'s fixture tests build no directories, and `t016_dir` is gone
- [x] `CfgFs`/`NoFile` are the shared helper rather than per-file re-implementations
- [x] T-077's ordering is settled in writing, so its fixtures aren't built twice
