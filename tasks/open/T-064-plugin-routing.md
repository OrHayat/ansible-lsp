# T-064 — Plugin routing: redirects, deprecations, tombstones (+ rename autofix)

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | M    | —          |

## Problem

Module names are not resolved by directory listing alone — there's a routing layer we
ignore entirely. Core ships `config/ansible_builtin_runtime.yml` (the 2.10 collection-split
table: every pre-collections bare name → its new FQCN home) and every collection may ship
`meta/runtime.yml` with `plugin_routing:`. Three record types (real entries, 2.21.2):

| Record        | Example                                              | Meaning                                    |
| ------------- | ---------------------------------------------------- | ------------------------------------------ |
| `redirect`    | `docker: redirect: community.docker.docker`          | renamed/moved; works, lives elsewhere      |
| `deprecation` | `removal_version: 15.0.0` + `warning_text`           | works today, dies at that version          |
| `tombstone`   | `stderr: tombstone: removal_version: 2.0.0` + text   | already removed; using it fails            |

Consequences of ignoring it: `community.general.docker` has **no file** in that collection
— it exists only as a redirect, so our resolver reports a working module as unknown (or
navigates nowhere). Every legacy bare name has the same problem via the builtin table. And
the redirect record is machine-readable "replace `xxx` with `yyy`" — a code-action autofix
we're leaving on the table.

## Approach

- Parse `plugin_routing` from core's builtin table and each reachable collection's
  `meta/runtime.yml` (installed + in-repo), cached per file like module doc schemas.
  Sections of interest: `modules` and `action`; the other plugin types can wait.
- Same table, fourth key: `plugin_routing.modules.<name>.action_plugin` declares which
  action plugin handles a module, **overriding** the same-name convention and checked
  first (`loader.py:673-674`, `task_executor.py:955-958`). T-029's module hover only
  checks the same-name twin, so it false-negatives on routed modules until this parser
  feeds it. (Core's builtin table has zero `action_plugin` entries — collections only.)
- Resolution follows redirects — **chained** (a → b → c happens across collection moves),
  with a cycle guard, before concluding "unknown module". Navigation lands on the final
  real file; hover can show the chain.
- Diagnostics: `deprecation` → WARNING quoting `warning_text` and `removal_version`;
  `tombstone` → ERROR (statically provable failure), quoting the replacement text.
- **Code action**: on a redirected or deprecated name, offer "replace with `<fqcn>`" —
  rewrite the task key span. First code action in the project; the span is the module key,
  which extraction already has.

## Traps

- A redirect target's collection may not be installed — that's the existing "uninstalled
  dependency" silence, not a new warning.
- `requires_ansible` also lives in runtime.yml — version gating is real but separate;
  don't let it grow this ticket.
- Bare short names consult the builtin table *and* `collections:` search lists — interacts
  with T-042 item 1; do the FQCN path first.
- Tombstoned-at-runtime differs by installed core version; we report against the installed
  tree, same as all module resolution.

## Progress

Redirect *resolution* landed with the T-042 bare-name work: core's table and per-collection
`meta/runtime.yml` tables are read (`install::module_redirect`, cached per file), and
`resolve_module` chases chains with a visited-set cycle guard — the loader's
`while`/`redirect_list` loop transcribed. Demo fixtures pin it: `demo.alpha.relay` resolves
through two hops to `demo/charlie/plugins/modules/relay.py`; `loop_a ↔ loop_b` terminates
quietly. Hover marks the rename: `` `demo.alpha.relay` → redirected to `demo.charlie.relay` ``.
Still open here: deprecation/tombstone diagnostics, the rename code action, and the
`action_plugin` key. (The original example `community.general.docker` redirects to
`community.docker.docker`, which that collection has since tombstoned — so it skips
quietly today; the pure-redirect box is pinned by the demo fixture instead.)

## Done when

- [x] a pure redirect resolves, navigates to the real file, and is not warned unknown —
      pinned by `chained_module_redirects_resolve_and_cycles_terminate`
- [x] a chained redirect resolves; a redirect cycle doesn't hang — pinned by the same test. Live-verified
      2026-08-03: Ansible errors with "plugin redirect loop resolving <name> (path:
      [...])" — report the same, ERROR severity, path in the message. (Roles cannot be
      routed at all — proven in T-022's scope check — so this is modules/plugins only.)
- [ ] deprecated names get a WARNING with the `warning_text`; tombstoned names an ERROR
- [ ] code action rewrites the old name to the redirect target
- [x] legacy bare names route through `ansible_builtin_runtime.yml` (landed in T-042's
      bare-name resolution)
- [ ] corpus gate: zero new warnings on `~/app/ansible`
