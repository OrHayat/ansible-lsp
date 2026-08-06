# T-089 — Indexed access into static list vars: no element support, out-of-bounds unflagged

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P3       | L    | T-099 | —          |

## Problem

A list-valued var works only as a whole. Given

```yaml
vars:
  list1: [apple, banana, fig]
```

`{{ list1 }}` hovers, navigates, and passes the undefined-variable check. But element
access is invisible to the extension:

- `{{ list1[0] }}` — hover/goto fire only on the `list1` characters; the `[0]` is dead
  space, and nothing shows *which* element you get. Hover shows the whole flattened list.
- `{{ list1[6] }}` — no bounds check, even though the list is fully static and the index
  is a literal. Live-verified on 2.21.2: this is a **fatal task error** ("object of type
  'list' has no attribute 6"), while `list1[0]`, the attribute form `list1.0`, and the
  negative form `list1[-1]` all run fine. So an out-of-bounds literal index on a
  statically-known list is provably broken code the editor stays silent about.

Root cause: there is no expression AST. `condition::variable_uses` is a tokenizer — it
emits flat name+span uses (`VarUse`, the spine of today's hover/goto/undefined checks)
and `list1[0]` tokenizes down to the bare name; the subscript is discarded, so nothing
downstream can reason about it.

## Approach

Model the access as structure, then type it:

```
{{ list1[6] }}  ->  Subscript(Var("list1"), Int(6))       expression AST
typeof(list1) = array[str, 3]                             from the static play-vars def
6 not in [-3, 3)  ->  diagnostic
```

Element hover/goto fall out of the same tree: the node under the cursor is the
`Subscript`, its resolved value is the YAML element node, which already has a span.

Two scopes for the AST — decide at pickup, both kept open:

- **Option A — postfix-access mini-AST**: parse just `ident` followed by chains of
  `[int]` / `[-int]` / `['key']` / `.attr` into `Var` / `Subscript` / `Attr` nodes;
  everything else stays with the tokenizer. Smallest thing that gives `operator[](x, 6)`
  and the typeof story; a seed the full grammar can grow from.
- **Option B — full Jinja expression AST**: filters with args, tests, operators,
  literals, ternaries. A real grammar; most of it buys nothing for this bug (the
  `| default()` softening is already detected textually), but it subsumes the tokenizer
  and every future expression feature instead of adding a second partial parser.

Sized L for option B; A alone is closer to M.

The "type checker" reduces to constant-typing statically-known defs: a literal YAML
sequence gives `array[T, n]`. Sound only under hard gating — effective definition is
single, untemplated, host-independent (same conservatism as T-056 known-literals);
otherwise inventory can swap the list and the length is a lie. Out-of-bounds is fatal at
runtime, so ERROR by the T-087 precedent. `VarUse` stays for the flat features until an
AST subsumes it — rewriting working hover plumbing is not this ticket.

Quoted string keys (`mydict['key']`) fall out of the same parse and set up the dict
analogue, but dicts are out of scope here.

## Done when

- [ ] `{{ }}` / `when:` expressions with element access parse into an expression AST
      (scope per the option chosen at pickup), covering `list1[0]`, `list1.0`,
      `list1[-1]` (all live-verified valid forms)
- [ ] hover anywhere on `list1[0]` (including the subscript) shows the element value for
      a statically-known list; goto jumps to the element
- [ ] a literal out-of-bounds index on a statically-known list gets an ERROR diagnostic;
      `# noqa` suppresses it
- [ ] templated, multiply-defined, host-scoped, or non-literal-index cases stay silent
- [ ] demo gains labeled GOOD/BAD cases and LSP-level tests pin them
