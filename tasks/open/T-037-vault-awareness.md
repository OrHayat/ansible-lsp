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

## Done when

- [ ] a vaulted file yields no `unparseable` error (probe says none fires today — pin it
      as a regression test) and no missing/undefined noise
- [ ] the vault-id label surfaces on hover
- [ ] `!vault` scalars count as defined vars, resolve go-to-definition, and don't
      produce "undefined" warnings (test a task `vars:` and a vars-file entry)
- [ ] definedness hedges instead of warning whenever a vaulted vars source is reachable,
      the hedge names the vaulted file, and opacity is per-path — playbooks that reach
      no vault keep full coverage
- [ ] a test fixture with a real vault header is pinned

Docs: https://docs.ansible.com/ansible/latest/vault_guide/index.html
