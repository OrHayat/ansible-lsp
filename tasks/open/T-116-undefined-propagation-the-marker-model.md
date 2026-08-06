# T-116 — Undefined propagation: the Marker model

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-114 | —          |

## Problem

We treat "undefined" as a property of a name. Ansible 2.19+ treats it as a **value that
propagates**, and the rules decide both where a diagnostic should be anchored and how severe
it is.

`Marker` extends Jinja's `StrictUndefined` (`_internal/_templating/_jinja_common.py:56`):

| Access | Behaviour | Cite |
| ------ | --------- | ---- |
| `marker[key]` | **self-propagates**, always | `:131-134` |
| `marker.attr` | **self-propagates** unless dunder | `:124-129` |
| anything else | `trip()` → `MarkerError` | `:104-107` |

So `{{ undefined_var.foo.bar[0] }}` produces **one** marker carrying the *original*
variable's context, not an error per hop. A diagnostic should anchor on the root name and
span the whole chain — which is also what T-089 needs to decide where an out-of-bounds index
is reported.

Severity is per-keyword, not global (`_marker_behaviors.py`):

- `FailingMarkerBehavior:30` — always raises. `when:`, module args.
- `ReplacingMarkerBehavior:47-86` — records, renders `<< error N >>`, emits one **aggregated**
  warning when the context exits. This is `name:` (`task.py:395-403`).
- `RoutingMarkerBehavior:94` — dispatches per type.

So an undefined variable in `name:` is a warning and the play continues; the same name in
`when:` is fatal. We currently make no such distinction.

**The T-037 finding.** The concrete subclasses are `UndefinedMarker`, `TruncationMarker`,
`CapturedExceptionMarker` and **`VaultExceptionMarker`** (`_jinja_common.py:213-262`). A
vaulted value that cannot be decrypted becomes a marker that propagates *exactly* like an
undefined variable. That is the mechanism behind T-037's "hedge definedness where a vaulted
source is reachable" — it is not a heuristic, it is what Ansible does.

**The suppression set is closed.** `default`/`d`, and the `defined`/`undefined` tests, are
the only things that consume a marker without tripping — they are the "overrides that require
special arg handling" (`filter/core.py:825-826`, `test/core.py:345-346`). Everything else in a
filter chain propagates. So `{{ x | default('y') }}` suppresses and `{{ x | mandatory |
default('y') }}` does not.

## Done when

- [ ] a use inside a subscript/attribute chain anchors on the root name, spanning the chain
- [ ] `default`/`d`/`defined`/`undefined` suppress; no other filter does
- [ ] severity follows the per-keyword behaviour — `when:` fatal, `name:` warning
- [ ] the vault case is expressed as marker propagation, and T-037 cites it rather than
      re-deriving it
