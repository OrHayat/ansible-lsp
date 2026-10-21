# T-037 — Vault awareness (so the parse/undefined rules stop lying)

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P1       | M    | —          |

## Problem

An `ansible-vault`-encrypted file is not valid YAML — it's a header line plus base64:

```
$ANSIBLE_VAULT;1.1;AES256
33623462...
```

**Probed 2026-08-09** (while filing what became the duplicate T-154), which corrects this
ticket's original premise: the body IS valid YAML — hex lines form one plain multiline
scalar, so the file parses (1 node, `Ast::Other`) and `unparseable` does **not** fire.
The live lie is `var-undefined`: a playbook with `vars_files: [secrets.yml]` where
`secrets.yml` is vaulted warns

```
`db_password` is never defined in any file reachable from this playbook — ...
```

on every use — the file is reachable and does define it. Same failure for vaulted
`group_vars/`/`include_vars` sources, i.e. exactly where security-conscious repos keep
secrets.

## The three postures

1. **Decrypt** — the LSP could find the password the way Ansible does
   (`vault_password_file`, config). Rejected by design: decrypted secrets in editor
   memory and LSP traffic, to gain diagnostics precision, is the wrong trade.
2. **Pretend the file is empty** — today's behaviour, the lie above.
3. **Opaque: know that we don't know** — the header alone proves "some unknown set of
   vars is defined here". This is the fix. Cost: along a vaulted path a genuine typo
   (`db_pasword`) is also conceded — both names are "maybe in the box". Scope the
   opacity per-path so playbooks that never reach a vaulted file keep full coverage,
   and make the concession name the box ("may be defined in vaulted `secrets.yml`").

## What's knowable vs not

- **Static:** a file is vaulted (the `$ANSIBLE_VAULT;<version>;<cipher>[;<vault-id>]` header),
  its vault-id label, and inline `!vault` tagged scalars (the var *name* is visible).
- **Opaque forever:** the decrypted contents. A vaulted vars file defines names we cannot see.

## Approach

- Detect the vault header (first line) and treat the file as **vaulted, not broken**: suppress
  `unparseable` and any missing/undefined diagnostics that depend on its contents. Hover:
  "Ansible Vault file (id: `prod`)".
- Detect `!vault` tagged scalars in otherwise-plain YAML: the key still counts as a *defined*
  variable for T-033; hover the value as "vaulted (id: …)".
- Where any reachable vars source is vaulted, T-033's "defined nowhere" must **degrade to a
  hedge** ("may be defined in a vault file") rather than warn — encrypted content could define
  the name.
- Optional: `vault_password_file` / `vault_identity_list` from `ansible.cfg` — resolve the
  path, warn if missing (extends closed T-003, which already parses the cfg).

## needs decision: hedge to what?

Reproduced 2026-10-20 on 2.21.3: `vars_files: [secrets.yml]` with `secrets.yml` wholly
vaulted, the play prints `db_password`, and `var-undefined` says it "is never defined in any
file reachable from this playbook". The typo `db_pasword_typo` beside it is reported the same
way and is genuinely undefined: Ansible fails on it.

The Approach above says definedness "degrades to a hedge rather than warn". T-228, which faces
the same opacity with vars plugins, says the opposite: an unknown name "keeps its warning with
the plugin conceded in the message". The two need one answer, since a reader cannot tell which
kind of opaque source is behind a warning. Options:

1. **Silence** every undefined name on a path that reaches a vaulted source. Never wrong, but a
   typo there is invisible.
2. **Keep the WARNING, name the vault** — "`db_password` is not defined in any file we can read;
   `secrets.yml` is vaulted and may define it." T-228's choice. Honest, but still a squiggle on
   working code.
3. **Downgrade to a HINT** naming the vault. Visible on request, never a warning on working code.

Whichever is chosen, the ignored test above already asserts the shared part and turns green
for any of them.

Upstream has no way to declare the names a vars file defines; the closest is an open 2018
design proposal, recorded in
[`upstream/ansible-proposal-redesign-vars.md`](../../upstream/ansible-proposal-redesign-vars.md).
Declarations exist only as role and play input signatures (T-041, T-240).

## Done when

- [ ] a vaulted file yields no `unparseable` error (probe says none fires today — pin it
      as a regression test) and no missing/undefined noise — the first half is pinned (the
      same test asserts a wholly vaulted file has no diagnostics at all); the second half is
      the decision below, with the ignored
      `a_name_from_a_vaulted_vars_file_is_never_called_undefined_without_naming_the_vault`
      waiting on it
- [ ] the vault-id label surfaces on hover
- [x] `!vault` scalars count as defined vars, resolve go-to-definition, and don't
      produce "undefined" warnings (test a task `vars:` and a vars-file entry) — already true,
      pinned by `vaulted_files_parse_clean_and_inline_vault_values_define_their_names` (play
      `vars:`, task `vars:`, vars file), red under a parser mutation that drops vaulted values
- [ ] definedness hedges instead of warning whenever a vaulted vars source is reachable,
      the hedge names the vaulted file, and opacity is per-path — playbooks that reach
      no vault keep full coverage
- [x] a test fixture with a real vault header is pinned — same test, real `ansible-vault`
      output for a throwaway password

Docs: https://docs.ansible.com/ansible/latest/vault_guide/index.html

## Config settings that change this (T-144 audit, ansible-core 2.21.2)

Precedence is env -> ini -> default; none of these declare a `vars`, `cli` or
`keyword` rung, so those do not apply (the T-098 pattern).

| setting | env | ini | what it changes |
| --- | --- | --- | --- |
| `DEFAULT_VAULT_PASSWORD_FILE` | `ANSIBLE_VAULT_PASSWORD_FILE` | `[defaults] vault_password_file` | The vault password file to use. Equivalent to ``--vault-password-file`` or ``--vault-id``. If executable, it w |
| `DEFAULT_VAULT_IDENTITY_LIST` | `ANSIBLE_VAULT_IDENTITY_LIST` | `[defaults] vault_identity_list` | A list of vault-ids to use by default. Equivalent to multiple ``--vault-id`` args. Vault-ids are tried in orde |
| `DEFAULT_VAULT_ID_MATCH` | `ANSIBLE_VAULT_ID_MATCH` | `[defaults] vault_id_match` | If true, decrypting vaults with a vault id will only try the password from the matching vault-id. |
| `DEFAULT_VAULT_IDENTITY` | `ANSIBLE_VAULT_IDENTITY` | `[defaults] vault_identity` | The label to use for the default vault id label in cases where a vault id label is not provided. |
| `DEFAULT_VAULT_ENCRYPT_IDENTITY` | `ANSIBLE_VAULT_ENCRYPT_IDENTITY` | `[defaults] vault_encrypt_identity` | The vault_id to use for encrypting by default. If multiple vault_ids are provided, this specifies which to use |
| `VAULT_ENCRYPT_SALT` | `ANSIBLE_VAULT_ENCRYPT_SALT` | `[defaults] vault_encrypt_salt` | The salt to use for the vault encryption. If it is not provided, a random salt will be used. |
