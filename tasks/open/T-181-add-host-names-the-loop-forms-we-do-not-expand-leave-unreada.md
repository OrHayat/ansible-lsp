# T-181 — add_host names the loop forms we do not expand leave unreadable

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P3       | M    | —          |

## Problem

T-179 made `unknown-host` read the hosts an `add_host` creates across include edges, so a
templated name over a **literal** loop is enumerable:

```yaml
- add_host:
    name: "{{ item }}-web"
  loop: ['a', 'b']          # -> a-web, b-web, measured
```

Three iteration forms are still unreadable, and each costs a **missed report** rather than a
wrong one — the name keeps a `{{` after substitution, so `hosts_unknowable` is set and the
rule goes quiet for the whole file:

| form | why it is not read |
| ---- | ------------------ |
| `loop_control: loop_var: node` | renames `item`, so `{{ node }}` never matches the substitution |
| `with_items:` and the other `with_*` | each runs a lookup plugin with its own semantics |
| `loop: "{{ some_var }}"` | the list is a variable; reading it is templating, not parsing |

The first is the one worth doing. `loop_var` is a rename and nothing more — `ast::build`
already reads `loop_control` for `unknown_keys`, so the value is one `get` away, and
`loop_items` is already on `Task`.

The third is out of scope on its own: resolving `loop: "{{ some_var }}"` means evaluating a
variable to a list, which is T-034's territory. Named here only so the boundary is written
down.

## Approach

- Carry `loop_var` on `Task` beside [`loop_items`], defaulting to `item`, and substitute that
  name. Measured on 2.21.2: `name: "{{ node }}"` with `loop_control: loop_var: node` over
  `loop: ['c']` creates host `c`.
- `with_items:` over a literal list is the same shape as `loop:` and is the only `with_*`
  worth considering — it is a plain list, not a lookup with arguments. Measure it before
  assuming that, and leave every other `with_*` alone.
- Keep the leftover-`{{` guard exactly as it is. It is what makes each of these safe to skip:
  an unhandled form leaves a template behind, and a template behind means silence.

## Traps

- Do not read `loop_items` without `looped`. Empty means *unreadable*, not *no iterations* —
  a consumer treating empty as zero would conclude a templated loop creates nothing.
- Expanding more names can only remove `unknown-host` errors, never add them, so the corpus
  gate can only fall. A rise means the substitution invented a name.

## Done when

- [ ] `loop_control: loop_var:` renames the substituted variable, with the default-`item`
      case asserted beside it so the rename is what is being tested
- [ ] `with_items:` over a literal list is measured first, then either expanded or recorded
      here as deliberately skipped with the measurement that decided it
- [ ] every unhandled form still silences the file, asserted per form rather than in one
      lump — the guard is the safety property and it deserves a test each
- [ ] corpus gate: `unknown-host` hits can only fall
