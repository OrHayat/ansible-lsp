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

So every rule that reads files trips on it. Most urgently, T-013 now publishes a red
`unparseable` **error** on any vaulted file — a false positive on every repo that uses vault.
`vars_files:`/`include_vars:` (T-016/T-017) pointing at a vault file, and T-033's "defined
nowhere" (a var may be defined *only* inside a vaulted vars file), are wrong the same way.

Vault is common. Without this, the loud diagnostics we just shipped are untrustworthy exactly
where security-conscious repos live.

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

- [ ] a vaulted file yields no `unparseable` error (and no missing/undefined noise)
- [ ] the vault-id label surfaces on hover
- [ ] `!vault` scalars count as defined vars and don't produce "undefined" hedged-away warnings
- [ ] T-033 hedges instead of warning whenever a vaulted vars source is in scope
- [ ] a test fixture with a real vault header is pinned

Docs: https://docs.ansible.com/ansible/latest/vault_guide/index.html
