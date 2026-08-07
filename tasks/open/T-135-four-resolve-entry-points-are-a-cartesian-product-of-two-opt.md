# T-135 — Four resolve entry points are a cartesian product of two optional axes

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P3       | S    | —          |

## Problem

`resolve.rs` exports one operation four times, once per combination of two optional inputs:

| Function | Line | Extra inputs |
| -------- | ---- | ------------ |
| `resolve(r, ctx)` | 271 | — |
| `resolve_in(r, ctx, fs)` | 278 | `fs` |
| `resolve_with(r, ctx, literals)` | 164 | `literals` |
| `resolve_with_in(r, ctx, literals, fs)` | 174 | both |

Two axes, `2² = 4` functions. The naming has already run out of room: `_with` means
literals and `_in` means fs, which nothing about the names says and no doc comment states
outright — `resolve_with_in` is unreadable without opening it. A third optional input makes
it eight, which is what stopped `RoleExts` from being threaded through the entry point in
T-091 (it went in as a value with a `Default` instead, reachable only from the call sites
inside `resolve_ref`).

Nothing is wrong today. Every one of the four is correct and tested. This is the API's shape
making the *next* change expensive.

## It has now cost something twice

Both times the workaround was to smuggle a resolver input in through a side channel, because
adding a parameter would have meant four more functions:

**`RoleExts` (T-091).** The role extension list became a value with a `Default`, reachable
only from inside `resolve_ref`. Tests can drive it directly, but nothing can drive it
end-to-end through `resolve`, so the list is pinned at unit level and the call-site wiring is
pinned separately. Complete coverage, split across two tests instead of one.

**`Reference::in_playbook` (T-095).** `{{ playbook_dir }}` means "this file's own directory"
in a playbook and "the invoking playbook's directory" anywhere else, so the resolver needs to
know which shape the file is. That is a property of the **file**, and it is currently stored
on every `Reference` in that file — N identical bools.

The other homes are all closed:

- `FileContext` is out on correctness, not taste: `cache.rs:272-280` keys its cache on
  `file.parent()`, so `site.yml` and `tasks.yml` in one directory share an instance. A
  per-file field there would be read by the wrong file.
- `extract` returning `{ in_playbook, refs }` makes extraction honest but changes nothing:
  `resolve_in` still receives a lone `&Reference`, so the flag has to be copied back onto
  each one. 24 call sites of churn for the same field.
- Recomputing in the resolver is impossible — `extract(nodes)` has the tree but not the
  path, `resolve_in` has the path but not the tree. That one bit is the only thing that
  has to cross, and `Reference` is the only channel.

So `in_playbook` is on `Reference` because **`resolve_in`'s unit of work is one reference and
nothing else is file-scoped**. With `Resolver` it becomes a field alongside `fs`, built once
per file, and the flag comes off `Reference` in the same change. T-137 later widens it to a
`Vec<PathBuf>` of chain-derived dirs, which is a field on a per-file struct and would be
absurd duplicated per reference.

Both cleanups are part of this ticket's payoff, not follow-ups.

## Approach

A params struct, so an axis costs a field instead of a function:

```rust
pub struct Resolver<'a> {
    pub fs: &'a dyn Fs,
    pub literals: Option<&'a HashMap<String, Vec<String>>>,
    pub exts: RoleExts,
    /// The file being resolved is a playbook, so `{{ playbook_dir }}` is `ctx.file_dir`
    /// exactly. Per file, which is why it does not belong on `Reference` (T-095), and
    /// why T-137 can widen it to a set without touching every reference.
    pub in_playbook: bool,
}

impl Default for Resolver<'static> {
    fn default() -> Self {
        Self { fs: &StdFs, literals: None, exts: RoleExts::default(), in_playbook: false }
    }
}

impl<'a> Resolver<'a> {
    pub fn resolve(&self, r: &Reference, ctx: &FileContext) -> Resolution { ... }
}
```

`&StdFs` const-promotes to `'static`, so `Default` needs no lazy static. Call sites read
`Resolver::default().resolve(&r, &ctx)`, or `Resolver { fs: &cache, ..Default::default() }`
when a scan is passing its memo. Three of the four entry points get deleted; `RoleExts` gets
an end-to-end test seam it doesn't have today (T-091 covers it at unit level plus two
end-to-end tests on the default, which is complete but split).

Edit surface, counted rather than estimated — 19 production call sites:

| Where | Sites |
| ----- | ----- |
| `ansible-lsp/src/main.rs` | 12 — hover, go-to-definition, diagnostics |
| `ansible-core/src/vars.rs` | 2 |
| `mutation.rs`, `bin/scan.rs` | 2 |
| `resolve.rs` internal | 3 (plus the 4 definitions collapsing into 1) |

The 16 calls in `resolve.rs`'s test module nearly all go through the local `resolve_src` /
`resolve_in` helpers, so they collapse into editing those two.

The only part needing thought is `main.rs`: those 12 are on request-handling paths and some
already hold a `ScanCache` they hand over as `fs`, so the struct must stay cheap to build per
request — by-reference fields, no clone of the literals map.

Its own commit, not riding along with whatever motivates it. A pure API reshape that touches
the LSP crate should be reviewable as one thing.

## Done when

- [ ] one public entry point; `resolve_in` / `resolve_with` / `resolve_with_in` are gone
- [ ] a new optional input costs a field, not a function
- [ ] `RoleExts` is reachable end-to-end from a test, without a hand-revert
- [ ] `in_playbook` is off `Reference` and on the params struct, set once per file
- [ ] no per-request allocation added on the LSP paths
