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

## Approach

A params struct, so an axis costs a field instead of a function:

```rust
pub struct Resolver<'a> {
    pub fs: &'a dyn Fs,
    pub literals: Option<&'a HashMap<String, Vec<String>>>,
    pub exts: RoleExts,
}

impl Default for Resolver<'static> {
    fn default() -> Self { Self { fs: &StdFs, literals: None, exts: RoleExts::default() } }
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
- [ ] no per-request allocation added on the LSP paths
