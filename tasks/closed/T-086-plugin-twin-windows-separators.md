# T-086 — `plugin_twin` matches a POSIX substring, so it finds nothing on Windows

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| done   | P1       | S    | T-118 | —          |

Found while building T-072, which needed the same lookup and had to route around this one.

## Problem

`plugin_twin` (`crates/ansible-lsp/src/main.rs`) locates a module's controller-side twin by
string surgery on the winner's path:

```rust
if s.contains("/plugins/modules/") {
    return Some(PathBuf::from(s.replace("/plugins/modules/", "/plugins/action/")));
}
let i = s.rfind("/modules/")?;
```

`won` arrives with native separators. On Windows every one of those searches misses, so the
function returns `None` for a collection module whose twin exists — and `module_hover`'s
`(false, None)` arm then prints **"runs on the target host"** and links only the module.

That is the tool stating a fact that is false. For `debug`, `template`, `copy` and every
other documentation-only module the real logic lives in the action plugin, and the hover
both mislabels where the code runs and withholds the file you actually wanted to open. It is
the exact failure T-073 existed to fix, reintroduced by the platform it wasn't tested on.

`module_hover` normalises separators for its own use one line earlier —
`won.to_string_lossy().replace('\\', "/")` — but passes the raw `won` to `plugin_twin`, so
the two disagree about what a path looks like.

## Why no test caught it

The three hover tests that pass on Windows never reach this code:

| test | path it exercises |
| ---- | ----------------- |
| `hover_finds_cfg_dir_action_plugin_twin` | `legacy_action_twin`, which joins `PathBuf`s |
| `hover_shows_module_provenance_not_paths` | needs an Ansible install — **ignored** here |
| `hover_names_the_network_platform_plugin` | T-072's own component-walking lookup |

So the only coverage of the string-replace branch is behind an `#[ignore]` that a POSIX CI
box silently satisfies. Anything asserting on a real collection twin would have failed on
Windows from the day it was written.

## Approach

Walk path components, as `network_platform_twin` now does — `dir.file_name() == "modules"`
and its parent `"plugins"`, then `dir.with_file_name("action")`. Separator-agnostic by
construction rather than by remembering to normalise at each call site.

The core layout (`<install>/ansible/modules/x.py` -> `<install>/ansible/plugins/action/x.py`)
is a second shape and needs its own arm; it is the one the ignored test covers.

Worth checking in the same pass whether any other `contains("/`…`/")` on a native path has
the same latent break — `module_hover`'s own `s.find("/ansible_collections/")` and
`s.contains("/plugins/action/")` are on the normalised string and are fine, but the codebase
should be swept rather than assumed.

## Audit result (done-when 3)

The sweep found one more real stray and a bonus bug in `plugin_twin` itself, both fixed:

- **Fixed** — `plugin_twin`'s `is_action` arm used `str::replace`, which returns the input
  unchanged on no-match; the caller's `.is_file()` filter then passed (the winner *is* a
  file) and the hover listed the same path as both "action plugin" and "module". The
  component walk returns `None` on shape mismatch; `plugin_twin_rejects_shapeless_paths`
  pins it.
- **Fixed** — `resolve.rs` split-table test asserted
  `.to_string_lossy().contains("community/docker/…")` on a native path (would fail on
  Windows iff `community.docker` is installed). Now normalised via `posix_display`, like
  the sibling assertion already was.

Everything else is safe: `module_hover` searches run on the pre-normalised `s`;
`short_plugin_path` goes through `posix_display` first; the `ends_with("a/b")` checks
throughout are `Path::ends_with`, which is component-based; `include_vars.rs`'s
`starts_with('/')` is deliberate (POSIX control-node semantics, documented in place);
the `split('/')`/`strip_prefix("~/")` calls in glob/config/resolve/include_vars act on
authored YAML/cfg strings, never on filesystem paths; the JS harnesses only slice
`file://` URIs, which are POSIX by construction.

## Done when

- [x] a collection module with a same-name twin hovers "runs on the controller" and links
      the plugin, in a test that runs (not skips) on Windows
- [x] the core `ansible/modules/` shape still resolves its twin
- [x] remaining native-path substring searches audited, and either fixed or recorded here as
      safe with the reason
