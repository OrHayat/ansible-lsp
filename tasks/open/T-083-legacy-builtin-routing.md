# T-083 — `ansible.legacy` is unmodelled and `ansible.builtin` skips the routing table

| Status | Priority | Size | Depends on           |
| ------ | -------- | ---- | -------------------- |
| open   | P1       | M    | refs T-042/1, T-064  |

Raised from an outside review, then checked against this repo's code. The review is quoted
first and unedited, because the parts of it that turned out to be already-correct here matter
as much as the parts that found holes.

## The review, as received

> Bare `debug:` resolves as `ansible.legacy.debug` — always. Legacy is the entry point for every
> unqualified name; it's not conditional on whether an override exists. Whether it ends up at
> core's `debug` is a separate question from what namespace it resolved through:
>
> | you write | resolves under | lands on |
> | --------- | -------------- | -------- |
> | `debug:` | `ansible.legacy.debug` | `library/debug.py` if it exists, else core |
> | `ansible.legacy.debug:` | same | same |
> | `ansible.builtin.debug:` | `ansible.builtin.debug` | core, always |
>
> So "is it builtin or legacy" conflates two things. The name is legacy. The file is usually
> core's, because legacy falls through to builtin when nothing local shadows it.
>
> `yum` is a worse case for your resolver, because there's a third layer: redirects. The current
> `ansible.builtin.yum` isn't a module at all — it's a redirect shipped in ansible-core. It points
> at `dnf` (the yum name is kept as an action-plugin alias for syntax compatibility; the actual
> YUM backend was dropped in ansible-core 2.17).
>
> That redirect table is `lib/ansible/config/ansible_builtin_runtime.yml` in ansible-core. Your
> LSP needs to load it. It contains both:
>
> - internal redirects (`yum` → `dnf`)
> - out-of-core redirects — a large set of pre-2.10 short names that now point into collections
>   (`ec2` → `amazon.aws.*`, `postgresql_*` → `community.postgresql.*`, etc.)
>
> So the full unqualified-name resolution is: local `library/` → `collections:` keyword → builtin
> → then the routing table, which may bounce you into an installed collection or to a "removed"
> tombstone entry. In an old codebase you'll hit those redirects constantly, and go-to-definition
> should follow them rather than dead-end at `ansible.builtin`.

And, on being asked whether legacy → builtin is itself a routing-table entry:

> It isn't a routing-table redirect. Two different mechanisms, and for your resolver that
> distinction matters — one is data you can parse, the other is loader behavior you have to
> hardcode.
>
> **Legacy → builtin is the plugin loader's search path.** `PluginLoader`
> (`lib/ansible/plugins/loader.py`) handles `ansible.legacy` specially: it does a filesystem
> search over the legacy paths first (role/playbook-adjacent `library/`, `DEFAULT_MODULE_PATH`),
> and on a miss falls through to `ansible.builtin`. There's no YAML entry saying
> `legacy.debug → builtin.debug`; it's just "didn't find `debug.py` on disk, try core."
>
> Underneath, the collection loader
> (`lib/ansible/utils/collection_loader/_collection_finder.py`) backs this with synthetic
> packages. `ansible_collections.ansible.builtin` and `ansible_collections.ansible.legacy` don't
> exist on disk; an internal redirect loader maps imports of
> `ansible_collections.ansible.legacy.plugins.modules.debug` onto core's actual
> `ansible.modules.debug`. Same file object, two addressable names.
>
> **Builtin → collection is the routing table.** `ansible_builtin_runtime.yml` is real data with
> explicit `redirect:` / `tombstone:` / `deprecation:` entries. Legacy inherits these only
> transitively: it falls back to builtin, and builtin then consults routing. So `- yum:` is
> legacy → miss on disk → builtin → routing says `dnf`.
>
> For the LSP, that's:
>
> ```
> resolve(unqualified name):
>   1. legacy filesystem search   ← hardcoded rule, path-order sensitive
>   2. collections: keyword list  ← from the play/role
>   3. ansible.builtin package    ← real files in ansible-core
>   4. builtin routing table      ← parse ansible_builtin_runtime.yml, follow chains
> ```
>
> Step 4 chains can be multi-hop and can terminate in a tombstone (removed, no target). Handle
> both, and note that a redirect target living in a collection the repo doesn't have installed is
> a legitimate diagnostic rather than a resolution failure on your end.

## What checking the code found

Read against `resolve.rs` and `main.rs`. **Not run** — module resolution needs an Ansible install
and the dev machine has none, so every item below is from the source and each names what would
confirm it.

**The mechanism split is already modelled correctly, and that is worth recording.** `resolve_module`
(`resolve.rs:435-447`) builds the legacy dirs and the core package into a *single candidate list*
and takes the first file that exists — which is the filesystem fall-through, not a redirect — then
consults routing only when that list misses (`:444-446`). Steps 1 → 3 → 4 in the right order with
the right semantics. The loop at `:432-493` already chases chains with a visited-set cycle guard,
so multi-hop is covered. The review's step 2 (`collections:` keyword) is genuinely absent, and is
already open as **T-042 gap 1**; tombstones and deprecations are already **T-064**.

So the holes are narrower than the review implies, but they are real, and two of them make the
tool go silent on names that work in production.

## The four issues

### 1. `ansible.builtin.X` never consults the routing table — `resolve.rs:468-470`

The 3-part arm always takes its redirect from `collection_module_redirect`, which looks for
`<collection_root>/ansible/builtin/meta/runtime.yml`. That file does not exist — `ansible.builtin`
is synthetic, exactly as the review says. Only the bare arm calls `builtin_module_redirect`. The
special case at `:462` covers the *module lookup* for `ansible.builtin` but not the *redirect*.

| written | routing consulted | result |
| ------- | ----------------- | ------ |
| `yum:` | yes | resolves to `dnf` |
| `ansible.builtin.yum:` | **no** | dead-ends |

One module, two spellings, two answers — and the FQCN spelling, the one every linter pushes people
toward, is the one that fails.

### 2. `ansible.legacy.X` does not resolve at all — `resolve.rs:462`

The special case is `if (ns, coll) == ("ansible", "builtin")`. Nothing handles `ansible.legacy`, so
a 3-part legacy name is treated as an ordinary collection, finds no
`<root>/ansible/legacy/plugins/modules/debug.py`, and skips. Per the review's own table,
`ansible.legacy.debug:` is identical to bare `debug:` and must resolve identically — legacy dirs
first, then core.

### 3. Internal redirects resolve with no visible seam — `main.rs:1032`

The `→ redirected to` hover line is gated on the winner's path containing `/ansible_collections/`.
`yum → dnf` stays inside core, so the winner has no collection path and the note never fires. Once
issue 1 is fixed, `ansible.builtin.yum:` would open `dnf.py` with nothing saying why. The
out-of-core redirects (`ec2 → amazon.aws.*`) do show the hop; the internal ones are silent — the
exact class of unmarked seam that note was added for.

### 4. The namespace label is read off the winning file — `main.rs:983-995`

`module_hover` derives the namespace from where the file landed, so a bare `debug:` that falls
through to core is labelled `ansible.builtin`. The review's point stands: the *name* is legacy, the
*file* is core's. The existing test name concedes it —
`hover_labels_workspace_library_modules_legacy` gets `ansible.legacy` only when a workspace
`library/` wins.

This is not cosmetic. `ansible.builtin.debug` can never be shadowed; `debug:` can, by dropping one
file into `library/`. A hover that calls them both builtin hides the difference that decides
whether a local override is possible.

## Done when

- [ ] `ansible.builtin.yum:` resolves to `dnf` through the routing table, same as bare `yum:`
- [ ] `ansible.legacy.debug:` resolves exactly as bare `debug:` does — legacy dirs first, core
      second — pinned against `demo/library/ping.py`, which already proves the shadowing half
- [ ] a redirect that stays inside core shows the hop on hover, like a redirect into a collection
      does
- [ ] the hover distinguishes the namespace a name resolved *under* from the file it landed *on*,
      so `debug:` and `ansible.builtin.debug:` do not read identically
- [ ] each of the four is pinned by a fixture that does not need an Ansible install, or is marked
      `#[ignore]` with the reason, consistent with T-077
