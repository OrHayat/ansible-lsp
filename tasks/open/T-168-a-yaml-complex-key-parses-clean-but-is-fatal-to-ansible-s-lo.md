# T-168 — A YAML complex key parses clean but is fatal to Ansible's loader

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P1       | S    | —          |

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

Flag any non-scalar mapping key at parse/walk level — a mapping or sequence used as a
key is fatal to Ansible everywhere, since every consumer constructs Python dicts.
Message should follow upstream's hint (braces around a template block need quotes),
since that is what the spelling almost always means.

Check whether sequence-valued keys (`? [a, b]` explicit-key form too) reach the same
constructor error before claiming they do — only the flow-mapping-as-key form is
measured so far.

## Done when

- [ ] the symptom file gets an error diagnostic anchored on the complex key
- [ ] the message mentions quoting the template block, like upstream's hint
- [ ] a quoted `"{{ x }}":` key stays clean (it is valid and templates in module args)
- [ ] the sequence-as-key and explicit `?` forms are measured and covered or documented
      as out of scope
