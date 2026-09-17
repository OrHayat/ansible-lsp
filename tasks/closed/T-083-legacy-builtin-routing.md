# T-083 — `ansible.legacy` is unmodelled and `ansible.builtin` skips the routing table

| Status | Priority | Size | Epic  | Depends on          |
| ------ | -------- | ---- | ----- | ------------------- |
| done   | P1       | M    | T-118 | refs T-042/1, T-064 |

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
confirm it. *Since run: see "Measured 2026-09-17" below, which corrects issues 1 and 2.*

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

*Wrong on its first row, measured below: bare `yum:` dead-ended too. Its rename lives in the
table's `action:` section, and the parser read only `modules:`.*

### 2. `ansible.legacy.X` does not resolve at all — `resolve.rs:462`

The special case is `if (ns, coll) == ("ansible", "builtin")`. Nothing handles `ansible.legacy`, so
a 3-part legacy name is treated as an ordinary collection, finds no
`<root>/ansible/legacy/plugins/modules/debug.py`, and skips. Per the review's own table,
`ansible.legacy.debug:` is identical to bare `debug:` and must resolve identically — legacy dirs
first, then core.

*Not identical, measured below: a bare name searches the `collections:` list first and
`ansible.legacy.X` does not.*

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

## Measured 2026-09-17

ansible-core 2.21.3. "Ansible" is `ansible-playbook` output — a `library/` module returning a
marker, `-vvv` "Using module file", or `--syntax-check` with every collection hidden
(`ANSIBLE_COLLECTIONS_PATH` at an empty dir, `ANSIBLE_COLLECTIONS_SCAN_SYS_PATH=False`). "Before"
is our resolver against the same install via `AnsibleInstall::detect(None)`.

| written                              | Ansible                                     | before     |
| ------------------------------------ | ------------------------------------------- | ---------- |
| `ansible.legacy.ping`, `library/ping.py` present | `library/ping.py`                | skipped    |
| `ansible.builtin.ping`, same         | core `ping` (`library/` ignored)             | core, ok   |
| `ansible.legacy.ufw` / `ansible.builtin.ufw` | `community.general.ufw` via core's table | skipped |
| `ufw` / `ansible.legacy.ufw`, `library/ufw.py` present | `library/ufw.py`, before the table | bare ok, legacy skipped |
| `yum`, `ansible.builtin.yum`, `ansible.legacy.yum` | resolve, collections hidden     | all skipped |
| `ansible.builtin.normal`             | passes (action plugin only, no module file)  | —          |
| `ansible.builtin.nonsense_xyz`       | `couldn't resolve module/action`, play never starts | skipped, silent |
| `action: ansible.builtin.nonsense_xyz` | earlier task runs, then `Cannot resolve …` | skipped, silent |

Three corrections to the issues above, each with its control:

- **`yum` is an action rename.** `yum: redirect: ansible.builtin.dnf` sits under
  `plugin_routing.action`; `_get_action_context` (`mod_args.py:59-66`) accepts a task name if the
  module *or* the action loader resolves it. It is the only `action:` rename that stays in core.
- **`ansible.legacy.X` skips the `collections:` list.** Under `collections: [demo.probe]`, which
  ships a `ping`, bare `ping:` ran the collection's and `ansible.legacy.ping:` ran `library/`.
  Control: without the list, bare `ping:` ran `library/` too.
- **`ansible.builtin` never reads a collection root.** An `ansible_collections/ansible/builtin`
  tree on `COLLECTIONS_PATH` is ignored; the same layout under `demo/probe` ran. The old 3-part arm
  searched there.

Added to scope on the way, since routing is what makes it safe: an `ansible.builtin.X` that core
has on neither disk nor table is a WARNING (`unknown-builtin-module`) naming the installed
version. Not the two-part name's ERROR — the answer is only as good as the core we read, and the
machine that runs the play may have a newer one. `ansible.legacy.X` and bare names stay silent:
library paths we cannot see can supply them.

After the change, the same probe against the real install agrees with the Ansible column on every
row.

A side finding, filed separately as T-237: when a collection is in both `~/.ansible/collections`
and the package's bundled `ansible_collections`, we pick the copy Ansible does not run.

## Done when

- [x] `ansible.builtin.yum:` resolves to `dnf` through the routing table — and so do bare `yum:`
      and `ansible.legacy.yum:`, which did not either; the table's `action:` section is read
      (`builtin_and_legacy_spellings_follow_cores_rename_table`)
- [x] `ansible.legacy.X` resolves as bare `X` does minus the `collections:` list — legacy dirs,
      core, then core's table — pinned in memory with the list as the control
      (`the_ansible_legacy_spelling_is_the_bare_search_without_the_collections_list`)
- [x] `ansible.builtin.X` reads only the package (`modules/`, `plugins/action/`) and core's table
      (`the_ansible_builtin_spelling_reads_only_the_package`)
- [x] an `ansible.builtin.X` core does not have is `unknown-builtin-module`, a WARNING naming the
      version and the failure its spelling produces; silent with no readable install, for an
      uninstalled redirect target, and for every other spelling
      (`an_ansible_builtin_name_core_does_not_have_is_missing`,
      `an_unknown_builtin_is_a_warning_naming_the_installed_core`)
- [x] a redirect that stays inside core shows the hop on hover, like a redirect into a collection
      does — read from `Resolution::redirects` (T-133) instead of guessed from the winning path,
      which also stops a `collections:` list hit from reading as a redirect
- [x] the hover distinguishes the namespace a name resolved *under* from the file it landed *on*,
      so `debug:` and `ansible.builtin.debug:` do not read identically — see the note below on
      what the distinction turned out to be
- [x] each of the four is pinned by a fixture that does not need an Ansible install:
      `demo/module_prefixes.yml` against a hand-built core
      (`the_module_prefixes_demo_matches_its_annotations`), plus the in-memory resolver tests
      above. The one guard that needs the real install is
      `every_other_demo_file_is_free_of_unknown_builtin_module_diagnostics`, which skips
      without one (T-203)

## Closing notes

**Box 4 is a lookup line, not a relabel.** Issue 4 argued a bare `debug:` should read as
`ansible.legacy`. Measured on 2.21.3, Ansible's own `resolved_fqcn` for `debug` and
`ansible.legacy.debug` is `ansible.builtin.debug` whenever the file comes from core (and `ping`
when `library/ping.py` wins) — so the existing `ansible.builtin` label agrees with Ansible, and
relabelling would contradict it. What differs is the search, so the hover adds: "looked up as
`ansible.legacy`: `library/` dirs are searched before the Ansible install, and
`ansible.builtin.debug` skips them".

It claims the lookup and nothing more, because "a local file would replace it" is false for
some modules. Measured: with `library/debug.py`, `debug:` still ran core's action plugin; with
`library/copy.py`, core's `copy` action plugin ran and shipped the local `copy.py`.

**A test that never ran.** `hover_marks_the_split_table_redirect` discovered its context
without an install, so `docker_container` never resolved and it returned before its assertion
on every machine. It attaches the detected install now, runs here, and was seen red with the
redirect line removed. Without `community.docker` installed it still returns early (T-203).
