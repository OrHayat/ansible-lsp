# T-080 — Resolution-aware FQCN suggestion that exempts local (`ansible.legacy`) modules

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P3       | M    | T-042/T-064, T-025, T-010 |

## Idea

Be *stronger and more accurate* than ansible-lint's `fqcn` rule by using something ansible-lint
doesn't have: the resolved target. ansible-lint nags "qualify this name" fairly bluntly — it
often can't tell a builtin from a collection module from a local `library/` module, so it fires
uniformly, and the noise on local modules is why people `# noqa` it.

This server already resolves each bare name to the file that actually wins. So it can offer the
**per-target-correct** fix — and, critically, **relax the requirement on local modules by
construction**, because it *knows* they're local:

| Resolved target                          | Suggestion |
| ---------------------------------------- | ---------- |
| `.../ansible/modules/debug.py` (builtin) | `ansible.builtin.debug` |
| a collection tree (`community.docker`)   | `community.docker.docker_container` |
| a local `library/` dir (`ansible.legacy`)| **exempt** — no "wrong" nag; optionally offer `ansible.legacy.<name>` to *pin* the resolution |
| unresolved                               | nothing — never guess |

## Design (matches the "don't nag correct code" stance)

- **Off by default.** This is a style/consistency aid, not a provable failure, so it must not
  fire out of the box. It's a designated toggle under **T-025** (`fqcn-suggest: hint`), and a
  **code action** ("Add FQCN") on the reference.
- The suggestion text is *always the resolved FQCN*, so it can never be wrong the way a
  string-guess can — if we can't resolve it, we say nothing (T-007/T-051 discipline).
- Local `ansible.legacy` modules are exempt from any "should be qualified" framing. The only
  thing offered there is the opt-in `ansible.legacy.<name>` pin, labelled as "pin resolution",
  not "fix a mistake".
- `# noqa: fqcn-suggest` (line and prior-line), rule id matched exactly (T-010).

## Done when

- [ ] with the toggle on, a builtin/collection bare name offers its exact resolved FQCN as a
      code action; applying it rewrites the task key
- [ ] a local `library/`/role-`library/` module is **never** told it's wrong; at most it offers
      `ansible.legacy.<name>` as an explicit pin
- [ ] unresolved names offer nothing
- [ ] off by default; `# noqa: fqcn-suggest` silences it; pinned by fixtures over
      `demo/tasks/modules.yml` (collection + builtin) and `demo/library/` (legacy)
