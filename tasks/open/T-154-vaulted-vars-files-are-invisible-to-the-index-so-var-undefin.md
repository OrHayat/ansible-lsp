# T-154 — Vaulted vars files are invisible to the index, so var-undefined lies

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | bug  | P1       | S    | —          |

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

(Inline `!vault |` values are NOT this bug: the key stays plaintext, so the name indexes
fine and only the value is opaque.)

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
- [ ] `var-uncovered-when` and hover degrade the same way (concede, don't claim)
- [ ] the T-150 matrix row for vault files points here instead of "won't do"
