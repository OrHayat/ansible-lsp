# T-210 — meta/main.yml accepts 22 keys that do nothing, and we say nothing

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P3       | M    | T-106 | T-209      |

## Problem

`RoleMetadata`'s legal set is 27 keys, and only **5** were designed for the file:
`allow_duplicates dependencies galaxy_info argument_specs` from `metadata.py:37-40`, plus
`collections` from `CollectionSearch`. The other **22** arrive from `Base`
(`base.py:686-721`) because `RoleMetadata(Base, CollectionSearch)` inherits the
FieldAttribute *machinery*, and `fattributes` — which `_validate_attributes` uses as its
legal set — is computed mechanically over the MRO (`base.py:95-105`). Nobody decided
`become:` should be allowed in `meta/main.yml`; the class hierarchy decided it.

So the file accepts `become: true`, `connection: ssh`, `environment: {...}`, `port: 22` and
eighteen more, and does nothing with any of them. `Role._load_role_data` reads exactly three
things off the loaded object — `_metadata.collections` (`role/__init__.py:286`),
`_metadata.dependencies` (`:469`) and `argument_specs` via `getattr` (`:343`).

T-147 made the *illegal* keys an error. This is the other half: keys that are legal, inert,
and pure noise in the file. A role author writing `become: true` there believes something
will become someone.

### Measured on core 2.21.3

Each key placed in `meta/main.yml` with an observable effect, then the identical key moved to
the play as the control — a probe that cannot produce "applied" proves nothing (rule 2):

| key            | probe                                     | in `meta/main.yml` | on the play (control) |
| -------------- | ----------------------------------------- | ------------------ | --------------------- |
| `become_method`| `doesnotexist`, play must fail            | inert (`ok=1`)     | **applied** (`failed=1`) |
| `vars`         | `{probe: FROM_META}`, task prints it      | inert              | **applied**           |
| `environment`  | `{PROBE: FROM_META}`, task echoes `$PROBE`| inert              | **applied**           |
| `connection`   | `doesnotexist`, play must fail            | inert              | **applied**           |
| `timeout`      | `1` against `command: sleep 3`            | inert              | **applied**           |
| `check_mode`   | `true`, `file:` must not create           | inert              | **applied**           |
| `no_log`       | `true`, output must be hidden             | inert              | **inert** — see below |

Six of seven measured with a control that came out different. **`no_log` is unmeasured**: it
stayed inert on the play too, so that probe cannot tell the two apart and proves nothing in
either direction. Do not carry it into the rule as "inert" — measure it properly first, or
leave it out of the flagged set.

The remaining 15 `Base` keys are untested. Nothing suggests they differ, and that is exactly
the assumption this ticket exists to stop making: `Play` and `Task` inherit the same 22 and
several of them very much work there.

## Approach

A **hint**, never a warning — the file is valid Ansible and loads fine, so anything louder
misrepresents it. Same tier reasoning as T-021's faded `unused-file`.

Message names the mechanism rather than scolding: `become:` is accepted here because
`RoleMetadata` inherits it from `Base`, and nothing reads it. Autofix: delete the key and its
value, which is why this is blocked on [[T-209]] — today's span covers the key token only.

**Go over all 27, one at a time.** The rule's flagged set is the measured-inert set, not
"everything not in the own-keys list". Two keys already break that shortcut: `collections`
*is* inherited and *is* read (`role/__init__.py:286`), and `name` is a `Base` key that is a
role's own name in other contexts. Guessing the set from the class hierarchy is how this
becomes a hint that lies about the two keys that matter.

## Done when

- [ ] every one of the 27 keys measured in `meta/main.yml` with a probe whose control comes
      out different, and the table above completed — inert, read, or unmeasurable, with the
      probe recorded for each
- [ ] `no_log` re-probed with an observable that discriminates, or explicitly excluded and
      said so
- [ ] the hint fires on a measured-inert key at HINT severity, and `collections:` and
      `dependencies:` stay silent — the two that prove the set was measured, not derived
- [ ] the autofix deletes key *and* value, asserted on a key with a nested block
- [ ] its own rule id, `# noqa`-suppressible per T-010
- [ ] a demo row beside T-147's in `demo/roles/metadata-keys/meta/main.yml`, whose `become:`
      row is currently labelled a deliberate silence — that label becomes wrong the day this
      lands, and `the_role_metadata_demo_reports_exactly_its_bad_rows` must be updated with it
- [ ] corpus gate: how many hits across the four trees T-147 named. A hint that fires on
      every published role is noise, and the answer decides whether this ships at all
