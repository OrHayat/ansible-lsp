# T-039 — `requirements.yml` ↔ installed-collections cross-check

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P2       | M    | —          |

## Problem

The #1 real-world onboarding failure: a playbook uses `community.docker.docker_container`, the
collection isn't installed, and it blows up at runtime with "couldn't resolve module". The
contract lives in `requirements.yml` (galaxy) but nothing checks it against what's actually
resolvable:

```yaml
# requirements.yml
collections:
  - name: community.docker
  - name: ansible.posix
```

## Approach

`install.rs` already knows the installed collections (that's how module FQCNs resolve today).
Cross that with the FQCNs used in playbooks:

- FQCN used, collection **not** in any `collections_path` → warn "collection `X` not installed"
  — and say whether it's declared in `requirements.yml` ("listed but not installed" is an
  `ansible-galaxy install` away; "used and undeclared" is a missing dependency).
- `requirements.yml` entries: local `src:` paths → resolvable (go-to-def + missing check);
  galaxy names and git URLs are remote → opaque.

## Traps / limits

- Install location varies per machine/venv (`ANSIBLE_COLLECTIONS_PATH`); the diagnostic must
  read "not found in *these* paths", never assert the collection doesn't exist anywhere.
- Don't warn on `ansible.builtin` / already-installed collections.

## Done when

- [ ] an FQCN whose collection isn't installed is flagged, distinguishing declared-vs-undeclared
      in `requirements.yml`
- [ ] local `src:` requirements resolve + missing-check; remote entries stay silent
- [ ] the message names the paths searched
- [ ] no false positive on installed or builtin collections

Docs: https://docs.ansible.com/ansible/latest/collections_guide/collections_using_playbooks.html
