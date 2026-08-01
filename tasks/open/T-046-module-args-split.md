# T-046 — Harden the module/args split (ModuleArgsParser semantics)

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | M    | T-044      |

## Problem

The AST's `find_action` is best-effort: the module is "the first non-directive key", plus the
first token of `action:`/`local_action:`. That's enough for the resolvers today, but the
variable-*use* work wants the module's args cleanly separated from Ansible keywords, and the
value-naming forms aren't fully parsed. Ansible does this in one place —
`ModuleArgsParser.parse()` (`lib/ansible/parsing/mod_args.py`).

## Approach

Mirror the parser's real handling:

- **Free-form** modules (`command: echo {{ x }}`, `shell:`, `script:`, `raw:`) — the value is a
  command string, not a mapping; its `{{ }}` still reference variables.
- **`action:` / `local_action:`** — `"module key=val key2={{ y }}"`: strip the module name,
  the rest is args (T-046 currently keeps the whole string as the "args").
- **`args:`** — merges extra parameters into whatever module the task named.
- Normalise so callers get `{ module, args }` regardless of form.

## Traps / limits

- `command`/`shell` free-form + `args:` can coexist — merge, don't overwrite.
- Keep FQCN handling (`ansible.builtin.command`) — the split is on the *value*, not the key.
- Must not change reference extraction — gate with the `scan` snapshot.

## Done when

- [ ] free-form, `action:`/`local_action:`, and `args:` forms all yield a normalised module+args
- [ ] the args subtree/string is exposed for variable-use extraction (T-049 consumers)
- [ ] `scan` output unchanged (module reference detection identical)
- [ ] pinned tests per form
