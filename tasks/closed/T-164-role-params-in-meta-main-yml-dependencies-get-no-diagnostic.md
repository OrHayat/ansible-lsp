# T-164 — Role params in meta/main.yml dependencies get no diagnostic

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P2       | S    | T-099 | T-147      |

## Problem

T-100 landed the `role-param-not-keyword` rule on a play's `roles:` entries. A
`meta/main.yml` `dependencies:` entry is the **same object** — `_load_dependencies`
"returns a list of RoleInclude objects" (`metadata.py:59-62`), so it runs the same
`_split_role_params` — and gets nothing.

Live-verified on 2.21.2, re-measured on 2.21.3 with a control (`include_role:` with the same
`tasks_from:` does run `alternate.yml`):

```yaml
# roles/app/meta/main.yml
dependencies:
  - role: web
    tasks_from: alternate.yml    # does not load alternate.yml
    becom_user: root             # does not become anyone
```

Role `web`'s `tasks/main.yml` runs, and the role sees `tasks_from=alternate.yml` and
`becom_user=root` as variables. Identical to T-100's repro, one file over.

## Approach

The rule and its near-miss derivation already exist (`attributes::role_param_problems`,
`KeyContext::RoleDefinition`); what is missing is a caller. `meta/main.yml` parses as
`Ast::Other`, so `attributes::problems` never looks at it — which is exactly the routing
T-147 builds, hence the dependency rather than a second path-sniffing site.

`references::meta_dependencies` already reads these entries and already handles the
`name:` alias, so the extraction half is done; it needs the param split beside it.

Watch the galaxy requirement form (`metadata.py:70-80`): a `dependencies:` entry may be
`{src: …, version: …, scm: …}` rather than a role name.

**Correction, measured on 2.21.3:** `src` and `version` are *not* kept out of the role — they
reach it as variables (`src=web version=1.0`). They are silent only because neither names a
keyword. And a `tasks_from:` written beside `src:` is a variable too, so the galaxy form is
split the same way and warns the same way. The form exists only under `dependencies:`: the
same entry in a play's `roles:` dies with "role definitions must contain a role name".

## `user:` — three meanings, one of them a false warning

Running the rule over the corpus found **8 hits in kubespray `46dbdd3` — 4 distinct, in 3 files;
the tree symlinks its role dir — every one on correct code**:

```yaml
# roles/etcd/meta/main.yml
dependencies:
  - role: adduser
    user: "{{ addusers.etcd }}"
```

`adduser` reads that variable (`name: "{{ user.name }}"`). The rule fired because
`legal_key(Play, "user")` is true, and `param_suspicion` took that as "a keyword on a play".
It is only a preprocess escape — `Play.preprocess_data` renames it (`play.py:166-174`) — and
it means something else in each position. Measured on 2.21.3, each with a control:

| where               | what `user:` is                        | evidence                                                                           |
| ------------------- | -------------------------------------- | ---------------------------------------------------------------------------------- |
| a play              | the SSH login, deprecated `remote_user` | `-vvv`: `ESTABLISH SSH CONNECTION FOR USER: t164_login_probe`; without it, `None`. `{{ user }}` is undefined |
| a task              | the builtin `user` module               | `TASK [user]` runs it (`--check`, ok); beside `debug:` it is `conflicting action statements: debug, user` |
| a `roles:` entry    | a variable for the role                 | the role sees `user=t164_roles_probe`; SSH user stays `None`                       |
| a `dependencies:` entry | a variable for the role             | the role sees `user=t164_dep_probe`; SSH user stays `None`                         |

So on a role entry `user:` is an ordinary param, exactly like `port_count:`. The rule must not
treat a preprocess escape as a keyword; only real attributes count. This was already a false
positive on a play's `roles:` entries since T-100 — this ticket is where the corpus found it.

## Found while doing this, not fixed here

- **T-236.** A param on a play's `roles:` entry is indexed as a variable definition: hover and
  go-to-definition on `{{ app_env }}` in `app_dir: /etc/{{ app_env }}` find `app_env: staging`
  on the same entry. The same entry under `meta/main.yml` `dependencies:` gets **neither** —
  measured through the real binary, both `null`. `vars.rs` reads params only from `Play.roles`.
- **T-235.** Running the real binary on kubespray for the sweep below also showed
  `missing-file` on `etcd_defaults` and `network_plugin/calico_defaults` — roles that exist
  with no `tasks/main.yml`, which Ansible loads without complaint.

## Done when

- [x] `tasks_from:` on a `meta/main.yml` `dependencies:` entry warns, same rule id and
      message shape as the play-level case — `keyword_shaped_dependency_params_warn`, the
      message reading "on a dependencies: entry" where the play case says "on a roles: entry"
- [x] a genuine dependency param does not warn, and neither does the `src:`/`version:`
      galaxy form — `ordinary_dependency_params_and_keywords_stay_silent`,
      `the_galaxy_dependency_form_splits_the_same_way` (which also pins that a play's `roles:`
      has no galaxy form)
- [x] a `dependencies:` key in a file that is not a role's `meta/main.yml` is untouched —
      `a_dependencies_key_outside_role_metadata_is_not_role_params`, with the `meta/` copy of
      the same text as the control that does fire
- [x] a demo fixture carries both halves — `demo/roles/dependency-params/meta/main.yml` (a
      meta file has to sit in a role), pointed to from `demo/role_params.yml`'s header, pinned by
      `the_dependency_params_demo_reports_exactly_its_bad_rows` and exempted by path suffix from
      `every_other_demo_file_is_free_of_role_param_diagnostics`. Every row was run on 2.21.3
- [x] `user:` on a `roles:` entry and on a `dependencies:` entry is silent, with a keyword-shaped
      param beside it that still warns — `user_on_a_role_entry_is_a_variable`. Fixed by
      `keywords::is_attribute`, which is `legal_key` without the preprocess escapes; `user` on
      a play is the only escape in the tables, so nothing else moved
- [x] a play's `user:` is still legal (no `invalid-attribute`) and a task's `user:` is still
      read as the module — `user_on_a_play_is_legal`, `user_on_a_task_is_the_user_module`
- [x] both demos carry the kubespray shape as a GOOD row, and the corpus sweep is re-run:
      0 hits on kubespray

Each new test was seen red first: with the dependency check removed, with the `src:` fallback
disabled, with the fallback leaked into a play's `roles:`, and with `legal_key` put back in
place of `is_attribute`.

## Corpus sweep

`role-param-not-keyword` over **every** file, not only `meta/main.yml`, since the `user` fix
also changes the `roles:` side. Run through a temporary edit of `role_metadata_corpus`, not
k| tree                | files walked | before the `user` fix           | after                    |
| ------------------- | ------------ | ------------------------------- | ------------------------ |
| `demo/` (control)   | 122          | —                               | 7 — exactly its BAD rows |
| kubespray `46dbdd3` | 1258         | 8 (4 distinct), all `user:` (*) | 0                        |
| debops              | 4910         | 0 (*)                           | 0                        |
| openstack-ansible   | 1715         | 0 (*)                           | 0                        |
| community.general   | 1265         | 0 (*)                           | 0                        |
| ansible-examples    | 151          | —                               | 0                        |
| sovereign           | 68           | 0 (*)                           | 0                        |
| ansible-role-mysql  | 32           | 0 (*)                           | 0                        |

(*) measured over `meta/main.yml` files only, before the sweep was widened.
