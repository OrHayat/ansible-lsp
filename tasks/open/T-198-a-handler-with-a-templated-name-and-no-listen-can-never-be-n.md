# T-198 — A handler with a templated name and no listen: can never be notified

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | S    | T-099 | T-028      |

## Problem

A handler's `name:` **is** templated, unlike its `listen:`. Measured on 2.21.2:

| handler                        | `notify: restart nginx` |
| ------------------------------ | ----------------------- |
| `name: "restart {{ svc }}"`    | matches, handler runs   |
| `listen: "restart {{ svc }}"`  | **fatal** — braces stay in the topic |

That templating happens per host, at notification time, from
`search_handlers_by_notification` (`plugins/strategy/__init__.py:474-497`). When it fails,
ansible-core skips the handler and — only if it has **no** `listen:` topics — warns:

> Handler '%s' is unusable because it has no listen topics and the name could not be templated
> (host-specific variables are not supported in handler names)

So a handler named with a variable the play cannot resolve is dead weight: it can never be
reached by any `notify:`, and its `listen:` topics are the only way in. Ansible says so at run
time, per host, and only when something notifies — which is the same visibility problem as
[[T-196]]. The editor can say it while the handler is being written.

The flip side of [[T-196]]'s condition 2: there, one templated handler name forces silence
across the whole scope, because it could match anything. Here the same construct is the thing
being reported. Doing this first makes that suppression cheaper to explain — the scope is only
poisoned by names that *might* resolve, and this rule names the ones that never will.

## Approach

Report a handler whose `name:` carries a template and that declares no `listen:` topic. Two
distinct verdicts, and the difference is the whole rule:

- **the name uses a variable no reachable definition supplies** — provably unusable, matching
  ansible-core's own warning. This is the diagnostic.
- **the name uses a variable that is defined** — usable, and it is [[T-196]]'s wildcard rather
  than a fault. Not reported here.

Which means this rule is a consumer of the variable-definedness work ([[T-051]], epic
[[T-112]]), not a string check. Shipping it as "the name contains `{{`" would fire on every
correct templated handler in the corpus, which is the false-positive shape this project exists
to avoid.

A handler with a template in its name **and** a `listen:` topic is fine and must stay silent —
that is the documented escape, and it is the reason ansible-core's own warning carries the
`if not handler.listen` guard.

## Done when

- [ ] a templated handler name with no reachable definition and no `listen:` is reported
- [ ] a templated handler name **with** a `listen:` topic is silent, asserted separately — this
      is the guard ansible-core itself has, and dropping it makes the rule wrong not noisy
- [ ] a templated handler name whose variable *is* defined is silent
- [ ] the measured `name:`-templates / `listen:`-does-not asymmetry is pinned by a test, since
      the whole rule rests on it
- [ ] `# noqa` suppressible, per T-010
- [ ] demo fixture rows for all three outcomes, per rule 4
- [ ] seen red before the fix, per rule 5
- [ ] corpus gate: how many handlers carry a templated name at all, and how many of those have
      no `listen:` — recorded here
