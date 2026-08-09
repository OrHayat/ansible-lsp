# T-144 — Audit base.yml: every setting that touches what we model, routed to its ticket

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | S    | T-090 | —          |

## Problem

T-098 proved its env-var list complete only for the settings `config.rs` consumes *today* —
and in doing so found two misses (`ANSIBLE_ACTION_PLUGINS`, `ANSIBLE_HOME`) in a ticket that
thought it had three. The same blind spot exists at the next level up: ansible-core has
~100+ settings in `base.yml`, and nobody has checked which of the others affect something we
model or plan to model. Each one we miss is a config a user can set that silently makes our
picture wrong — the T-090 divergence, one setting at a time.

Known candidates already spotted while closing T-098, none yet recorded in their ticket:

| Setting(s)                                              | Belongs to |
| ------------------------------------------------------- | ---------- |
| `DEFAULT_VAULT_PASSWORD_FILE`, `VAULT_IDENTITY_LIST`    | T-037      |
| `ANSIBLE_INVENTORY` / `DEFAULT_HOST_LIST`               | T-062      |
| `DEFAULT_FILTER_PLUGIN_PATH`, `DEFAULT_TEST_PLUGIN_PATH` | T-115     |
| `DEFAULT_LOOKUP_PLUGIN_PATH`                            | T-038      |
| `DEFAULT_HASH_BEHAVIOUR` (changes how var dicts merge)  | T-051/T-112 |

## Approach

One mechanical sweep, not new machinery. For each setting in `base.yml` (2.21.2 — record the
version, per T-114's rule): does it change anything the extension computes or has an open
ticket to compute? Nearly all answer no (runtime-only: forks, callbacks, connection, become)
— those need only a skim. For each yes: add a line to the owning ticket naming the setting
and its env/ini hooks, following the precedence pattern T-098's Cause section documents
(env → ini → default; the extra `vars`/`cli`/`keyword` rungs fire only when declared, and
parse-time path settings declare none). A relevant setting with no owning ticket is a new
ticket, filed as part of this one.

## Done when

- [ ] every `base.yml` setting classified: runtime-only, or routed to an owning ticket
- [ ] each affected open ticket names its settings and their env/ini hooks
- [ ] settings relevant to nothing open are listed here with a one-line reason, so the next
      sweep starts from a recorded no rather than re-deriving it
- [ ] the ansible-core version audited is recorded
