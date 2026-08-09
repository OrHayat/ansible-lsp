# T-154 — Vaulted vars files are invisible to the index, so var-undefined lies

| Status       | Kind | Priority | Size | Depends on |
| ------------ | ---- | -------- | ---- | ---------- |
| **rejected** | bug  | P1       | S    | —          |

**Rejected as a duplicate**: T-037 (Vault awareness) already owned this finding — filed
without checking the open list first. The probe result, the three postures, and the
inline-vault analysis below were folded into T-037, including the correction that
`unparseable` does NOT fire (this file's body is a valid plain multiline scalar), against
T-037's original claim.

## Symptom

A playbook with `vars_files: [secrets.yml]` where `secrets.yml` is fully
vault-encrypted (`$ANSIBLE_VAULT;1.1;AES256` header — the standard way secrets ship,
also common for whole `group_vars/prod.yml` files) warns on every use of its variables:

```
`db_password` is never defined in any file reachable from this playbook — it may
still come from inventory, facts, or extra-vars (-e).
```

The file *is* reachable and *does* define it. Empirically reproduced during the T-150
matrix work: the vaulted body parses as one plain multiline scalar → `Ast::Other` → zero
definitions indexed → `var-undefined` fires. At runtime Ansible decrypts and the play is
fine — the diagnostic is false.

## What we can and cannot know

A fully-vaulted file is ciphertext of the *entire* YAML body — keys included. The variable
names inside are not hard to get, they are cryptographically absent; only the
`$ANSIBLE_VAULT;1.1;AES256` header line is plaintext. So there are exactly three postures:

1. **Decrypt** — the LSP could find the password the way Ansible does
   (`--vault-password-file`, config). Rejected by design: decrypted secrets in editor
   memory and LSP traffic to gain diagnostics precision is the wrong trade.
2. **Pretend the file is empty** — today's behaviour, and the lie this ticket fixes.
3. **Opaque: know that we don't know** — the header alone proves "some unknown set of
   vars is defined here". This is the fix.

The honest cost of opacity: in a playbook that reaches a vaulted source, a genuinely
typo'd var also goes unflagged — `db_password` (in the vault) and `db_pasword` (typo) are
both "maybe in the box". Per-path scoping keeps that cost local, and the concession
message should name the box (`may be defined in vaulted secrets.yml`).

**Inline vault is NOT this bug, and needs no concession.** With
`db_password: !vault |` only the *value* is ciphertext — the key is plaintext YAML, so
the name indexes normally, go-to-definition works, and `var-undefined` stays precise (no
rule reads the value anyway). Verify the parser keeps the `!vault` tag benign. The common
two-file convention degrades almost as well: a plaintext `vars.yml` mapping
`db_password: "{{ vault_db_password }}"` beside a vaulted `vault.yml` gives real
definitions for the public names, leaving only the `vault_*` names under the concession.
So whole-file vaulting takes the full degradation; the patterns designed to keep names
visible take nearly none.

## Cause

Nothing recognises the vault header. The vars index reads the decrypted-only-at-runtime
file as ordinary YAML, sees a scalar where a mapping would be, and honestly reports "no
definitions" — the dishonesty happens downstream, where `var-undefined` (and anything
else consuming reachability) treats "unreadable source" as "source with no definitions".

## Fix

Detect the header (first line starts `$ANSIBLE_VAULT;`) at the source-loading layer and
mark the file *opaque* rather than *empty*: a reachable opaque source means definedness
claims along that path must be conceded, exactly like the already-conceded inventory/-e
sources — `var-undefined` stays silent for anything a vaulted reachable file could
define. Go-to-definition on such names can land on the vaulted file itself. We never
decrypt: an editor holding vault passwords is out of scope by design.

## Done when

- [ ] a var used under a reachable vaulted `vars_files`/`include_vars`/`group_vars`
      source is not flagged `var-undefined`
- [ ] a vaulted file that is NOT reachable changes nothing — opacity is per-path, not
      global silence
- [ ] `var-uncovered-when` and hover degrade the same way (concede, don't claim), and
      the concession names the vaulted file
- [ ] an inline `!vault` value changes nothing: the key still indexes, defines its var,
      and resolves go-to-definition (test both a task `vars:` and a vars-file entry)
- [ ] the T-150 matrix row for vault files points here instead of "won't do"
