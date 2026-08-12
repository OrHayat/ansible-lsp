# T-171 — A hostvars host key is not a jump target

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | task | P2       | S    | —          |

## Problem

```yaml
msg: "{{ hostvars['web01'].web01_ib_ip }}"
#                   ^^^^^   ^^^^^^^^^^^
#                   host     variable — jumps since T-104
```

T-104 made the variable half navigable. The host half still does nothing: no hover, no
Cmd+click. Raised on the T-104 demo, where the row invites you to click a `hostvars[...]`
read and one of the two names silently declines.

The two are different kinds of name, which is why one was easy and the other was skipped:
`web01_ib_ip` is a variable and the variable index answers it, while `web01` is a *host* and
we keep no host index. It is also a string literal, and the expression scanner skips literal
contents on purpose — every quoted word would otherwise become a phantom variable.

## Approach

The full answer — which groups the host is in, every var it inherits — is T-062's, and needs
the inventory parsed. **This ticket is the half that does not.**

`host_vars/web01.yml` beside the playbook is a deterministic path, exactly like the
`group_vars/`/`host_vars/` directories [`read_var_dir`] already reads. The filename *is* the
host name, so matching the key to it requires no inventory at all:

- `hostvars['web01']` -> `host_vars/web01.yml` when that file exists, `.yaml` too
- no file, or a templated/variable key -> nothing, as now

Deliberately narrow. It answers "where are this host's variables written", not "what is this
host", and it must not grow into group membership — that is the T-062 boundary and the
reason this is filed separately rather than folded into the closed T-104.

Worth checking during: whether the same key should also become a `document_link`, so it
paints like a resolvable path. That is the `refs` pipeline rather than the definition
provider, and it may not be worth a new `ReferenceKind` for one shape.

## Done when

- [x] `hostvars['web01']` jumps to `host_vars/web01.yml` when the file exists
- [x] the `.yaml` spelling resolves too
- [x] a host with no `host_vars/` file, and a non-literal key (`hostvars[h]`), stay silent
      rather than guessing
- [x] the variable half keeps working on the same line — one click each, asserted together
- [x] a demo row in `demo/hostvars.yml`, pinned by a test
- [x] the key is **painted** like a resolvable path, not merely clickable
- [x] the tests exercise the assembled result, so deleting the wiring fails them

## Landed

`condition::hostvars_host_keys` (all literal keys) with `hostvars_host_key_at` as the
cursor-filtered view of the same scan, so what is painted and what is clickable cannot
disagree. `host_vars_file` resolves `.yml`/`.yaml`; `host_key_defs_at` answers Cmd+click and
`host_key_links` paints.

### The paint was missed, and so was the test that should have caught it

First cut wired the jump into `goto_definition` only. It worked and was invisible — no
colour, no underline, nothing saying the name could be clicked — and the first thing tried in
the editor was "'web01' isn't clickable". A feature nobody can find is not shipped.

The deeper fault is that the *test* did not catch it, and could not: it called
`host_key_defs_at` directly, so deleting the wiring broke nothing. Both trait bodies are now
split — `Backend::definition_at` and `Backend::document_links_of` — and the tests go through
those, covering the whole Cmd+click chain and the whole link list rather than one ingredient.
Verified by deleting each wiring line in turn: both now fail.

That is the general lesson, not a T-171 one: a test that calls the helper instead of the
assembly does not cover the assembly. It let two things through here in one sitting.
