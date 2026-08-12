# T-168 — A YAML complex key parses clean but is fatal to Ansible's loader

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | bug  | P1       | S    | —          |

## Symptom

```yaml
- name: set_fact
  ansible.builtin.set_fact:
    {{ result_name }}: true      # meant as a template, parsed as a complex key
```

The tool is silent — `scan` reports `0 unparseable` — while `ansible-playbook` refuses
the file at load:

```
[ERROR]: YAML parsing failed: This may be an issue with missing quotes around a
template block.
```

Live-verified on core 2.21.2 (this session's probe, `p20_setfact_unquoted.yml`). The
file can never run, and we say nothing — the exact failure mode the `unparseable` rule
exists to prevent, reached through a different layer.

## Cause

An unquoted `{{ x }}:` is valid YAML *syntax*: the braces open flow mappings, and the
inner mapping becomes a **complex key** — a non-scalar key, which YAML permits. libyaml
emits a clean event stream, so `Document::parse` succeeds and every downstream rule sees
a well-formed mapping whose key is not a scalar (`as_str()` is `None`, so rules skip it).

Ansible fails one layer up: its loader constructs Python objects from the same events,
a `dict` is unhashable as a `dict` key, and the constructor raises — wrapped in the
"missing quotes around a template block" hint because upstream recognises this as the
common spelling mistake. So the fatal check lives in construction, not in the grammar,
and we stop at the grammar.

## Fix

`complex_key.rs`: flag any non-scalar mapping key, at any depth, in every document kind
— error, rule id `complex-key`. Message is template-flavoured with the quoting fix when
the offending **line** carries `{{` (the key's own text misses `msg: {{ x }}`, where the
complex key is the brace-less inner `{ x }` — upstream's hint uses the same line-level
split), the plain complex-key truth otherwise.

All the remaining forms were measured, and the gap was wider than filed: our libyaml
parses **all five** spellings clean (`{{ x }}: v` in a playbook, the same in a vars
file via `vars_files:`, inline `[a, b]: v`, inline `{a: 1}: v`, explicit `? [a, b]`)
while Ansible refuses every one — the inline flow-collection forms in its *scanner*
(`Colons in unquoted values must be followed by a non-space character`), the explicit
`?` form in the constructor (`While constructing a mapping found unhashable key`), and
the braces forms get the `missing quotes around a template block` hint, which upstream
attaches to any YAML error on a line carrying `{{`. Alias keys stay a documented miss
(T-160): the anchor may hold a scalar, and judging it needs the substitution.

## Done when

- [x] the symptom file gets an error diagnostic anchored on the complex key
- [x] the message mentions quoting the template block, like upstream's hint
- [x] a quoted `"{{ x }}":` key stays clean (it is valid and templates in module args)
- [x] the sequence-as-key and explicit `?` forms are measured and covered or documented
      as out of scope
