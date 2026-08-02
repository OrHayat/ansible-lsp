# T-057 — Parse module `DOCUMENTATION` / `RETURN` for input & output schema

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | L    | T-046      |

## Problem

Today the model stops at names. Go-to-def resolves a module FQCN (`community.general.nmcli`)
to its file, and `register: result` records that `result` exists — but nothing knows what
**arguments** the module accepts or what **fields** it returns. So:

- `result.stdout` / `result.wrong_field` after a register are both opaque — no validation, no
  hover, no completion of sub-keys.
- A task's own parameters (`state:`, `path:`, a typo'd `pathh:`) aren't checked against what the
  module actually declares.

Every Ansible module carries its own machine-readable contract for both. We just don't read it.

## Research (why the docstrings, not the code)

Every module `.py` defines three module-level string constants, each holding **YAML**:

- **`DOCUMENTATION`** — the input contract: `options:` maps each parameter to `type`,
  `required`, `default`, `choices`, `aliases`, `description`.
- **`RETURN`** — the output contract: each top-level key is a returned field, with `type`,
  `returned` (under what condition it appears), `sample`, and `contains:` for nested sub-fields
  (so `stat.exists`, `result.results[0].item`).
- **`EXAMPLES`** — sample task YAML, display-only, not parsed for meaning.

```python
RETURN = r'''
stat:
    type: dict
    returned: success
    contains:
        exists: { type: bool, returned: always }
        size:   { type: int,  returned: success, path exists }
'''
```

**This is exactly what `ansible-doc` reads** — it loads the module, pulls those constants,
parses them as YAML, prints them. It never runs or analyzes the code.

**Why not read the code instead?** For *inputs* it's largely moot — `DOCUMENTATION.options` is
authoritative and also what `AnsibleModule(argument_spec=...)` is built from. For *outputs*,
reading the code is only *partially* possible: keys given as literals in `exit_json(stdout=...)`
or `result['exists'] = ...` are statically visible, but modules build the result dict
imperatively and finish with dynamic forms —

```python
result[key] = v          # variable key
result.update(other)     # keys from elsewhere
module.exit_json(**facts)# spread; keys unknown statically
```

— plus keys sourced from a subprocess/JSON at runtime. So a code scan yields a **silently
incomplete** set with no signal for which modules fell short. `RETURN` is complete-by-intent and
trivially parseable; it's the right primary source, and it's why every tool uses it.
Caveat: `RETURN` is hand-written docs — it can be stale, thin, or absent (legacy/community
modules), which is what T-058 handles.

**One `DOCUMENTATION` wrinkle:** it supports `extends_documentation_fragment:` /
`doc_fragments` — shared option blocks pulled from other files and merged in. `ansible-doc`
does this merge; a faithful parser must too, or common options (e.g. connection args) go missing.

## Approach

- Locate the module file the way go-to-def already does (FQCN → collection path / `ansible_source`).
- Extract the `DOCUMENTATION` and `RETURN` string constants (regex/AST for the assignment) and
  parse each as YAML. Merge `extends_documentation_fragment` for `DOCUMENTATION`.
- Build an input schema (option → type/required/default/choices/aliases) and an output schema
  (field tree from `RETURN` + `contains:`), keyed by module file, cached like external var files.
- Wire consumers:
  - **task args**: hover/completion for parameters; soft hint on an unknown parameter.
  - **register output**: attach the output tree to that `VarSource::Register` def so `X.field`
    can hover/complete, and `X.<not-a-field>` is a soft hint (walk `contains:` for nesting).

## Traps / limits

- Docs can lag code — everything derived from them is a **hint**, never a hard error. Absence of
  a field ≠ error (dynamic returns exist). This is the whole reason T-058 exists.
- `EXAMPLES` is not a contract — ignore for validation.
- `RETURN`/`DOCUMENTATION` can themselves be pulled from a sidecar `.py`/adjacent file in some
  collections; start with the in-file constant.
- Nested access (`result.results[0].item`) needs `contains:` traversal and list handling.

## Done when

- [ ] a module FQCN resolves to a parsed input schema (`options`) and output schema (`RETURN`)
- [ ] `extends_documentation_fragment` is merged for inputs
- [ ] task parameters hover/complete from `DOCUMENTATION`; unknown param → soft hint
- [ ] a registered var's sub-keys (`result.stdout`) hover/complete from `RETURN`, nesting via `contains:`
- [ ] schemas are cached per module file (rarely change), not re-parsed per request
- [ ] modules missing the docstrings degrade to nothing (→ T-058), never a false error

Docs: https://docs.ansible.com/ansible/latest/dev_guide/developing_modules_documenting.html
