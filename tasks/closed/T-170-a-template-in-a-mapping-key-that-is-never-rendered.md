# T-170 — A template in a mapping key that is never rendered

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | task | P2       | S    | —          |

## Problem

Found while measuring T-169, which asked which mapping keys Ansible templates. Answer:
almost none. `set_fact` and `set_stats`' `data:` render a key — upstream calls it "a rare
case where key templating is allowed" (`action/set_fact.py:44`) — and every other spelling
fails, in one of two ways. Measured on 2.21.2:

| spelling                                       | what happens                                        | tier    |
| ---------------------------------------------- | --------------------------------------------------- | ------- |
| `debug: {"{{ argname }}": x}`                  | fatal: `Unsupported parameters … {{ argname }}`      | error   |
| `add_host: {name: h, "{{ k }}": v}`            | runs; host var is literally named `{{ k }}`          | warning |

Both are the author reaching for a dynamic name and getting nothing. The second is the
worse one: `add_host` accepts arbitrary keys as host vars, so it looks exactly like a third
rendering site, and it neither errors nor warns. The intended variable is simply never set,
and the one that *is* set cannot be referenced from any expression. Verified by reading the
host's own vars back in a later play: `['{{ k }}']`.

This is the same shape as T-103 — a field where braces are text, split by severity into
"Ansible refuses to run" and "Ansible runs and misbehaves" — but a different mechanism:
T-103's four keywords are declared `static=True` upstream, while these are ordinary args
whose *keys* simply never reach the templar. And it is not T-168 either: that one is the
**unquoted** spelling, which dies in the YAML loader before any of this.

Today we say nothing about either.

## Approach

- The error half is a lookup we already have: if a key carries `{{` and the action is not
  one of the two rendering sites, the module rejects it. The risk is claiming this for a
  module that legitimately takes free-form keys, so the rendering sites and `add_host` must
  come from an enumerated list, not from a guess about the module.
- The warning half is `add_host` specifically. `group_by` takes a `key:` and is worth
  measuring beside it before writing either message.
- Restrict to **module arg keys**, at the top level of the args mapping. A key nested
  inside a value is ordinary data, where braces in a key are legal and mean nothing —
  measured, and already the boundary T-169's walk draws.
- Rule 2: the control that must come out different is `set_fact`/`set_stats`, which are
  correct and must stay silent. T-169's tests already pin them from the other direction.

## Landed (2026-10-04) — `templated-arg-key`, ansible-core 2.21.2

`crates/ansible-core/src/templated_key.rs`, wired into diagnostics in `main.rs`, demo in
`demo/templated_keys.yml`.

### The Approach's two behaviours turned out to be four

The ticket had the error half as "if a key carries `{{` and the action is not one of the two
rendering sites, the module rejects it". Rejection is right; one message for it is not.
Measured:

| spelling | 2.21.2 |
| --- | --- |
| `debug: {"{{ k }}": v}` | fatal, `Unsupported parameters for (debug) module: {{ k }}` |
| `group_by: {key: g, "{{ k }}": v}` | fatal, **`Invalid options for group_by: {{ k }}`** |
| `add_host: {name: h, "{{ k }}": v}` | runs; host var literally named `{{ k }}` |
| `command: {"{{ k }}": v}` | fatal, but the key is *dropped*: `one of the following is required: _raw_params, cmd, argv` |

So `group_by` is covered (box 5) and gets its own measured wording rather than the generic
one — it validates its own options instead of going through the module arg spec.

The free-form row is the one that changed the design. `command` and its family swallow the
unknown key and fail for an unrelated reason, so reporting `Unsupported parameters` there
would be a confident wrong answer *about a real failure* — the worst kind, because the user
goes looking for the wrong thing. They are skipped via an enumerated `FREE_FORM` list, and
the exclusion is asserted. The cost is a missed error on a rare spelling; the alternative was
a wrong message on it.

### The decisive control

`debug: {"{{ p }}": x}` with `p` bound to `msg`. If keys were rendered this becomes `msg:`
and prints. It fails with `Unsupported parameters for (debug) module: {{ p }}` — so nothing
was rendered, and the rule is not guessing at a mechanism.

### It corrected a demo label that was already wrong

`demo/add_host_vars.yml:63` read `NO HINT: a templated KEY defines nothing reachable`. That
is a claim about *our* output, and this rule warns there — so the label was false the moment
the rule shipped. It now reads `BAD (templated-arg-key)` and keeps the original point, which
was about the *variable* rules staying silent. The false-positive gate is what caught it: it
is pinned to that one line rather than skipping the file, so the file cannot grow a second
one unnoticed.

### Corpus gate

The 759-file corpus this ticket counted against is not on this machine, so the gate ran over
the one that is — kubespray, 584 files: **0 errors, 0 warnings**. A clean sweep means nothing
on its own, so the control: pointed at `demo/`, the same gate reports 2 errors and 2 warnings,
the four known rows. `templated_key_corpus` is `#[ignore]`d and re-runnable with
`ANSIBLE_CORPUS=<path>`.

The ticket's "39 templated keys in the tree" could not be re-counted. A naive grep for
templated keys over kubespray returns 8, and all 8 are text *inside block scalars* — two in
`defaults/` vars files, two inside a `set_fact` value — so none is a mapping key at all. Worth
knowing before anyone trusts that 39: the obvious regex counts block-scalar contents.

## Done when

- [x] a templated key on an ordinary module's args is an error naming the real failure
      (`Unsupported parameters`), not a generic templating remark
      — `a_templated_key_on_an_ordinary_module_is_an_error_naming_the_real_failure`
- [x] a templated key on `add_host` is a warning saying the host var takes the braces
      literally and the intended name is never set
      — `a_templated_key_on_add_host_is_a_warning_about_the_literal_name`
- [x] `set_fact` and `set_stats`' `data:` keys stay silent — the control, asserted
      — `the_two_rendering_sites_stay_silent`; seen red by emptying `RENDERS_KEYS`
- [x] a templated key nested inside a value stays silent, at every depth
      — `a_templated_key_nested_in_a_value_stays_silent_at_every_depth`; seen red by making
      the walk descend one level
- [x] `group_by` measured and either covered or recorded as out of scope with the result
      — covered, with its own measured wording (`Invalid options for group_by`)
- [x] demo rows for both tiers, and a corpus gate over the 39 templated keys in the tree
      — `the_templated_keys_demo_matches_its_annotations_exactly` asserts the exact set and
      both tiers; `every_other_demo_file_is_free_of_templated_arg_key_diagnostics` guards the
      rest of the tree. The 39 could not be re-counted (corpus absent) — see above
