# T-144 — Audit base.yml: every setting that touches what we model, routed to its ticket

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P2       | S    | T-090 | —          |

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
| `VARIABLE_PLUGINS_ENABLED`, `DEFAULT_VARS_PLUGIN_PATH`, `RUN_VARS_PLUGINS` | T-228 |
| `DEFAULT_<TYPE>_PLUGIN_PATH` for every type not yet read (ini `<type>_plugins`) | T-227 |
| `DEFAULT_STRATEGY`, `DEFAULT_BECOME_METHOD` — the cfg defaults behind T-109's keyword values | T-109 |
| `INVENTORY_ENABLED` (`[inventory] enable_plugins`) — unread, so a `yaml` source disabled there is still indexed | T-152 |
| `PLUGIN_FILTERS_CFG` — a rejectlist that blocks modules at load; a blocked module resolves fine for us. Unverified | T-118 |

## Approach

One mechanical sweep, not new machinery. For each setting in `base.yml` (2.21.2 — record the
version, per T-114's rule): does it change anything the extension computes or has an open
ticket to compute? Nearly all answer no (runtime-only: forks, callbacks, connection, become)
— those need only a skim. For each yes: add a line to the owning ticket naming the setting
and its env/ini hooks, following the precedence pattern T-098's Cause section documents
(env → ini → default; the extra `vars`/`cli`/`keyword` rungs fire only when declared, and
parse-time path settings declare none). A relevant setting with no owning ticket is a new
ticket, filed as part of this one.

## Recorded as runtime-only

From the plugin-type survey of 2026-09-07 (2.21.3), settings that change how a play runs and
nothing we compute: `CALLBACKS_ENABLED`, `DEFAULT_STDOUT_CALLBACK`, `DEFAULT_CALLBACK_PLUGIN_PATH`
and the callback family; `DEFAULT_SHELL_PLUGIN_PATH`, `DEFAULT_TERMINAL_PLUGIN_PATH`,
`DEFAULT_CLICONF_PLUGIN_PATH`, `DEFAULT_HTTPAPI_PLUGIN_PATH`, `DEFAULT_NETCONF_PLUGIN_PATH`;
`DEFAULT_CACHE_PLUGIN_PATH` and the `CACHE_PLUGIN_*` tuning (only `CACHE_PLUGIN` itself matters,
and is read). `DOC_FRAGMENT_PLUGIN_PATH` joins this list unless T-057 ever merges user-supplied
fragments. T-227's type table is the per-type register these came from.

## The audit — ansible-core 2.21.2, all 220 settings (2026-10-03)

`config/base.yml` from the installed 2.21.2 holds **220** entries (207 uppercase plus the
`_`-prefixed internals and the three `_Z_TEST_ENTRY` fixtures). Every one is classified below.

Completeness is checked, not asserted: the classification is held as a set per bucket and
diffed against the parsed `base.yml` key list, failing on anything missing, double-counted or
invented. That check is the whole point of this ticket — T-098 "proved" a list complete by
reading the consumers and missed two settings, so a sweep that ends in a hand-written list
would reproduce exactly the defect it was filed for. Result: 220 classified, 0 missing, 0
duplicated, 0 phantom.

| bucket | count |
| --- | ---: |
| already read by `config.rs` | 19 |
| routed to an open ticket | 51 |
| relevant, nothing open owned it — now filed | 3 |
| runtime-only | 147 |
| **total** | **220** |

### Already read (19)

`ANSIBLE_HOME`, `COLLECTIONS_PATHS`, `COLLECTIONS_SCAN_SYS_PATH`, `DEFAULT_ROLES_PATH`,
`DEFAULT_MODULE_PATH`, `DEFAULT_ACTION_PLUGIN_PATH`, `BECOME_PLUGIN_PATH`,
`DEFAULT_CONNECTION_PLUGIN_PATH`, `DEFAULT_STRATEGY_PLUGIN_PATH`, `CACHE_PLUGIN`,
`DUPLICATE_YAML_DICT_KEY`, `ERROR_ON_MISSING_HANDLER`, `INVALID_TASK_ATTRIBUTE_FAILED`,
`DEFAULT_HOST_LIST`, `INVENTORY_ANY_UNPARSED_IS_FAILED`, `INVENTORY_UNPARSED_IS_FAILED`,
`INVENTORY_UNPARSED_WARNING`, `DEFAULT_JINJA2_EXTENSIONS`, `NETWORK_GROUP_MODULES`.

### Routed to an open ticket (51)

Each ticket below now carries its own table naming the settings and their env/ini hooks, so
the routing is readable from the owning ticket rather than only from here.

| ticket | settings |
| --- | --- |
| T-112 Variable definedness | `DEFAULT_HASH_BEHAVIOUR`, `DEFAULT_PRIVATE_ROLE_VARS`, `INJECT_FACTS_AS_VARS`, `VARIABLE_PRECEDENCE`, `FACTS_MODULES`, `CONNECTION_FACTS_MODULES`, `DEFAULT_GATHERING`, `DEFAULT_MANAGED_STR` |
| T-114 Jinja and templating | `DEFAULT_JINJA2_NATIVE`, `DEFAULT_NULL_REPRESENTATION`, `STRING_TYPE_FILTERS`, the four `_TEMPLAR_*` |
| T-106 Keyword schema and value types | `DEFAULT_STRATEGY`, `DEFAULT_BECOME_METHOD`, `DEFAULT_TRANSPORT`, `ANY_ERRORS_FATAL`, `DEFAULT_FORCE_HANDLERS`, `ENABLE_TASK_DEBUGGER`, `TASK_DEBUGGER_IGNORE_ERRORS` |
| T-037 Vault awareness | `DEFAULT_VAULT_PASSWORD_FILE`, `DEFAULT_VAULT_IDENTITY_LIST`, `DEFAULT_VAULT_ID_MATCH`, `DEFAULT_VAULT_IDENTITY`, `DEFAULT_VAULT_ENCRYPT_IDENTITY`, `VAULT_ENCRYPT_SALT` |
| T-152 YAML inventory files | `INVENTORY_ENABLED`, `INVENTORY_IGNORE_EXTS`, `INVENTORY_IGNORE_PATTERNS`, `TRANSFORM_INVALID_GROUP_CHARS`, `DEFAULT_INVENTORY_PLUGIN_PATH` |
| T-228 Vars-plugin variables | `VARIABLE_PLUGINS_ENABLED`, `DEFAULT_VARS_PLUGIN_PATH`, `RUN_VARS_PLUGINS` |
| T-115 Filter/test/lookup index | `DEFAULT_FILTER_PLUGIN_PATH`, `DEFAULT_TEST_PLUGIN_PATH` |
| T-118 Collections and routing | `PLUGIN_FILTERS_CFG`, `COLLECTIONS_ON_ANSIBLE_VERSION_MISMATCH` |
| T-121 Conditional analysis | `ALLOW_BROKEN_CONDITIONALS`, `ALLOW_EMBEDDED_TEMPLATES` |
| T-057 Module DOCUMENTATION | `DOC_FRAGMENT_PLUGIN_PATH`, `DEFAULT_MODULE_UTILS_PATH` |
| T-038 File-hitting lookups | `DEFAULT_LOOKUP_PLUGIN_PATH` |
| T-017 `include_vars` | `YAML_FILENAME_EXTENSIONS` |
| T-116 Undefined propagation | `DEFAULT_UNDEFINED_VAR_BEHAVIOR` |
| T-229 vars_plugins_enabled | `PLAYBOOK_VARS_ROOT` |
| T-111 module_defaults | `VALIDATE_ACTION_GROUP_METADATA` |
| T-137 playbook_dir | `PLAYBOOK_DIR` |
| T-238 Collection subdirectories | `MODULE_IGNORE_EXTS` |

Two of the Problem section's guesses did not survive. `DEFAULT_FILTER_PLUGIN_PATH` and
`DEFAULT_TEST_PLUGIN_PATH` were listed against T-115 and still belong there, but the row
routing the per-type plugin paths to T-227 is spent — T-227 closed, and the paths it did not
take (`DEFAULT_CACHE_PLUGIN_PATH` and the callback/cliconf/httpapi/netconf/terminal family)
are runtime-only, recorded below rather than owned. `DEFAULT_LOOKUP_PLUGIN_PATH` → T-038 and
the vault and vars-plugin rows held exactly as written.

### Relevant, nothing open owned it — filed (3)

| setting | new ticket |
| --- | --- |
| `TAGS_RUN`, `TAGS_SKIP` | **T-244** — `[tags] run = a` drops every other task *including untagged ones*; measured |
| `HOST_PATTERN_MISMATCH` | **T-245** — flips a no-matching-host play between warning+exit 0 and error+exit 1; measured |

T-230 (tags on dynamic includes) and T-062 (inventory sources) are both closed and neither
covered the cfg side, which is why these had no home.

### Runtime-only (147)

Settings that change how a play runs, what it prints, or how `ansible-galaxy` installs, and
nothing the extension computes. Recorded by family so the next sweep starts from a recorded
no:

| family | n | why it cannot reach us |
| --- | ---: | --- |
| display, colour, diff, pager | 40 | output formatting of a run we never perform |
| connection, become, transport, ssh-agent, persistent | 29 | how Ansible reaches a target; parse-time sees no target |
| `ansible-galaxy` (`GALAXY_*`) | 19 | install-time; changes what is *on disk*, which we then read directly |
| execution and scheduling | 12 | forks, polling, timeouts, retry files |
| callback and cache plugins | 13 | the T-227 per-type register; none contributes a resolvable name |
| target-side module execution | 10 | interpreter discovery, tmp dirs, compression — all remote-side |
| logging | 8 | controller and target logging |
| internal and test-only | 9 | `_ANSIBALLZ_*`, `_MODULE_METADATA`, `COVERAGE_REMOTE_*`, `_Z_TEST_ENTRY*` |
| warning toggles | 5 | silence Ansible's own warnings, not ours |
| adhoc CLI | 3 | `ansible` / `ansible-inventory` argument defaults |

One judgement call worth recording rather than leaving implicit: `DOCSITE_ROOT_URL` is filed
runtime-only because we build documentation links from the install we detect, not from
Ansible's configured docsite root. If hover ever starts rendering a docs URL from config, it
moves out of this list.

## Done when

- [x] every `base.yml` setting classified: runtime-only, or routed to an owning ticket
      — 220 of 220, with the partition diffed against the parsed key list
- [x] each affected open ticket names its settings and their env/ini hooks
      — 17 tickets carry a "Config settings that change this" table
- [x] settings relevant to nothing open are listed here with a one-line reason, so the next
      sweep starts from a recorded no rather than re-deriving it
      — the runtime-only register above, by family; 3 orphans filed as T-244 and T-245
- [x] the ansible-core version audited is recorded — **2.21.2**
