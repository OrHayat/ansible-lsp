# T-119 — meta/runtime.yml has no schema validation anywhere

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | S    | T-118 | T-064      |

## Problem

A typo in a collection's `meta/runtime.yml` is a **complete no-op**. Not a warning, not a
`-vvv` line — the redirect simply never applies, and the first symptom is a module that
cannot be found for reasons nowhere near the file that broke it.

`_canonicalize_meta` is an empty function (`utils/collection_loader/_collection_finder.py:749-765`)
— relative redirects were never implemented — so every lookup is a plain `.get()` chain
against the raw dict. Nothing validates the keys. Consequences:

- `plugin_rounting:` (misspelled) does nothing
- `plugin_routing: {module: ...}` — singular, where the key is `modules` — does nothing
- an unknown plugin-type key under `plugin_routing` does nothing

The file is read **once**, at collection package import (`:705-736`), and a missing file is
silent (`:725-726`). It is parsed with `yaml.CBaseLoader`, so **every leaf is a string**:
`removal_version: 4.0` is `"4.0"` (`_collection_meta.py:31-33`).

A schema does exist — but only inside `ansible-test`, as a sanity check contributors run:
`test/lib/ansible_test/_util/controller/sanity/code-smell/runtime-metadata.py`. Valid plugin
types at `:240-260`, `import_redirection` allows only `redirect` (`:264-269`), `action_groups`
metadata allows only `extend_group` (`:275-287`). Anyone not running `ansible-test` on their
own collection gets nothing.

Adjacent, same file, same silence: `requires_ansible` mismatch is a **warning** by default
(`COLLECTIONS_ON_ANSIBLE_VERSION_MISMATCH`, `config/base.yml:289-296`), and a **malformed**
version spec is a warning where the constraint is then silently not enforced at all
(`loader.py:1699-1700`).

## Approach

T-064 parses this file for its own purposes; validating it while it is open is nearly free.
Transcribe the valid key sets from the `ansible-test` sanity check — that is the closest
thing to a published schema — and record which version they came from.

Applies to in-repo collections. For installed ones the warning is noise the user cannot act
on, so it should be scoped to collections inside the workspace.

## Done when

- [ ] unknown top-level keys and unknown `plugin_routing` plugin-type keys warn
- [ ] the singular/plural confusion (`module` vs `modules`) is called out by name
- [ ] a malformed `requires_ansible` spec warns that the constraint will not be enforced
- [ ] only workspace collections are checked
- [ ] the valid sets cite the `ansible-test` sanity check and its version
