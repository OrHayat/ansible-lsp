# T-046 — Harden the module/args split (ModuleArgsParser semantics)

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P2       | M    | T-123 | T-044      |

## Problem

The AST's `find_action` is best-effort: the module is "the first non-directive key", plus the
first token of `action:`/`local_action:`. That's enough for the resolvers today, but the
variable-*use* work wants the module's args cleanly separated from Ansible keywords, and the
value-naming forms aren't fully parsed. Ansible does this in one place —
`ModuleArgsParser.parse()` (`lib/ansible/parsing/mod_args.py`).

## Research (read against ansible-core 2.21.2, 2026-08-02)

Three things this ticket originally got wrong. All verified by reading the installed 2.21.2
tree, and the last one by a live `ansible-playbook` run.

**There are two sets, not one.** "Free-form" below conflates them:

| Set | Members | What it controls |
| --- | ------- | ---------------- |
| `FREEFORM_ACTIONS` (`constants.py:120`) | `command`, `raw`, `script`, `shell`, `win_command`, `win_shell` | the `check_raw` flag passed to `parse_kv` |
| `RAW_PARAM_MODULES` (`mod_args.py:49`) | the above **plus** `include_vars`, `include_tasks`, `include_role`, `import_tasks`, `import_role`, `add_host`, `group_by`, `set_fact`, `meta` | whether `_raw_params` survives into the task args (`task.py:207`) — otherwise it's popped and rejected |

**`parse_kv` runs on every string module arg**, not only free-form ones
(`mod_args.py:_normalize_parameters` → `parsing/splitter.py:parse_kv`). A token *without* `=`
always becomes `_raw_params`; `check_raw` only decides whether tokens *containing* `=` are
kept raw or parsed as options. `parse_kv`'s own docstring says non-`k=v` params are "simply
ignored" when `check_raw` is false — that is wrong, they are never ignored.

**Consequence, confirmed by running it:** a bare-scalar value containing `=` is split as a
key/value pair before the module ever sees it.

```yaml
- ansible.builtin.include_vars: vars/we=ird.yml
# ERROR: vars/we is not a valid option in include_vars
```

`find_action` treats that value as a path and reports it *resolved*. Ansible fails the task.
A wrong answer in the direction this project exists to avoid, so it needs a pinned test.

**The scalar is a k=v line, not a path — and that cuts both ways** (live-verified 2.21.2):

- `include_vars: x.yml name=db` **works**: `name=db` becomes a real option, `x.yml` is the
  non-`=` remainder that lands in `_raw_params`. Identical to `{ file: x.yml, name: db }`.
  Any option can ride along this way — and `dir=settings` flips which reference kind the
  task is, so extraction must parse the `=` tokens, not just strip them.
- Quoting does **not** protect a `=` path: `include_vars: "'we=ird.yml'"` still splits, into
  an option literally named `'we`. There is no way to pass an `=`-containing path free-form;
  only `file:` works.
- The `=`-token splitting is **universal** — every module whose value is a string goes
  through `parse_kv`, which is why 1.x-style `copy: src=a dest=b` works everywhere. Only the
  non-`=` *remainder* is gated on `RAW_PARAM_MODULES` (path/command for the allow-listed
  modules, rejected for everything else).

So for the include family the bare form has three sub-cases: no `=` anywhere → the whole
value is the path (today's behaviour, correct); `=` tokens present → parse them as options,
remainder is the path; a lone token containing `=` meant as a path → a provable task
failure, not a resolved reference.

**k=v values are strings forever** (live-verified, all three): action plugins get no
`argument_spec` coercion, so `depth=1` crashes the walk (`int > str` TypeError),
`ignore_unknown_extensions=false` is truthy (acts as true), `extensions=json` errors
("must be a list"). All statically provable from the args alone → they belong with the
provable-failure diagnostics (unknown param, dir/file mixing), which report at extraction
time regardless of what the runtime simulation says. Ported and pinned in
`include_vars.rs`; the lint itself is still to do.

## Approach

Mirror the parser's real handling:

- **Free-form** modules (`command: echo {{ x }}`, `shell:`, `script:`, `raw:`) — the value is a
  command string, not a mapping; its `{{ }}` still reference variables.
- **`_raw_params`** — the internal key a bare scalar lands in. Model both sets above, since
  membership decides whether a scalar value is a path (`include_tasks: f.yml`), a command
  string (`command: echo hi`), or an error.
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
- [ ] `FREEFORM_ACTIONS` and `RAW_PARAM_MODULES` modelled as separate sets
- [ ] a bare-scalar value containing `=` is not reported as a resolved path — pinned by a test
      quoting the real error (`vars/we is not a valid option in include_vars`)
- [ ] `=` tokens in a bare scalar are parsed as options, remainder as the path — pinned by
      `include_vars: x.yml name=db` (works) and `include_vars: dir=settings` (dir kind)
- [ ] quoting inside the scalar does not protect a `=` path — pinned by the `'we` error
