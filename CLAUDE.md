# Working rules

This tool's whole value is that it tells the truth about Ansible. A wrong answer costs more
than no answer — a false diagnostic trains people to ignore the squiggle, and a confident
wrong hover sends them to fix something that isn't broken.

Everything below is a rule that already got broken, with the incident that produced it.
They are cheap to follow and each one cost real time to learn.

## 1. A claim about Ansible is not true until it has been run

Reading the source is how you form a hypothesis. Running it is how you find out.

> `Role.get_vars()` combines the role params, then `combine_vars(all_vars, self.vars)` — the
> entry's `vars:` — *last*. Read plainly, later wins, so entry `vars:` beats a param.
> Measured: **the param wins.** Params are a separate published precedence level applied
> outside that bucket, which the function does not say.

The source gave a confident wrong answer, which is worse than an uncertain one. So: before
a behavioural claim lands in a message, a code comment, a ticket, or a test expectation,
run it against the installed core. The ticket prose already says "live-verified" and
"measured" everywhere — that is this rule, and it is not optional.

Reading upstream is still required. It tells you *what to measure* and *which edges exist*
— `_load_role_name` is where you learn `name:` is an alias at all. It just never settles
what happens.

**This applies to claims about our own tool, not only about Ansible.**

> T-178's Symptom read "hover and go-to-definition are the visible surfaces: both point at
> the inventory line as the definition of a variable that does not exist." It was never run.
> `is_injected` matches every name beginning `ansible_`, so both surfaces are silent for this
> key from *any* position, before the fix and after it. The ticket was rewritten twice on top
> of that one unmeasured sentence, and its acceptance box asked for an assertion that cannot
> hold — "still answers from a host position" describes something neither consumer ever did.
> The surface that *does* show the key, `path_substitution_hover`, turned up only by
> enumerating all six readers of the index and measuring each one.

Enumerating the consumers is rule 3's habit; the point here is that it settles a question
about our own behaviour that reading the call graph does not. "Can a test reach this code"
and "does this value reach that code" are different questions, and scoping only the first is
how a box gets written for a consumer that cannot answer.

## 2. A probe must be able to fail

Design the measurement so a wrong hypothesis produces a visibly different result. Then check
that it could have.

> To find out which spelling wins on a `roles:` entry, the obvious test is
> `vars: {port: 90}` against `port: 80`. It printed 90, which "confirmed" the wrong answer
> above. But `port` is one of `RoleInclude.fattributes` — it is the *connection port*, never
> a variable — so there was only ever one candidate and no collision to measure. The probe
> could not have produced the other answer.

Before believing a result, ask: what would falsify me, and can this setup produce it? Run
the control that must come out different. A measurement that cannot fail feels like
evidence and is not.

**This applies to the commands you verify with, not only the ones you measure with.**

> `cargo test --workspace | grep "test result" | head -3`, on a workspace with **seven**
> test targets, where the failing one was fifth. That command cannot print a failure. It
> reported green twice over a red `ansible-lsp` suite, and two commits went to origin
> broken on the strength of it.

Grep `^test result|FAILED`, never truncate a multi-crate summary, and read all of it before
committing. The rule above was already written down when this happened — a rule applied
only to the domain it was learned in is not yet a habit, which is the actual lesson and the
reason this sits here rather than in a rule of its own.

## 3. A rule about shared data belongs on the data, not in one caller

Enumerate the consumers before deciding where a check lives.

> Role params are scoped to their `roles:` entry. The scope check went into
> `undefined_uses` and not into hover, so the tool said `port_count` was "never defined"
> while the hover on the same token pointed at the definition. Two contradicting claims,
> one line apart.

The fix was to put `scope` on `Located` so every reader answers from one rule. When you add
a rule, list every read site and assert each one — a test per consumer, not per rule.

Corollary: check whether the rule you want is a *part* of an existing predicate rather than
the whole of it. Routing `undefined_uses` through `in_effect_at` also imported its
`set_fact`-ordering rule and broke the documented "any reachable definition exempts, even a
later one". Hence `in_scope_at` and `in_effect_at` are separate.

## 4. A demo label is a claim, so pin it with a test

`demo/` uses **GOOD** / **BAD** / **SILENCED** / **NO HINT** prefixes. Each is an assertion
about what the tool does, and hand-written assertions rot.

> A row was labelled `NO HINT: role params do not leak past the role`. That described
> *Ansible's* behaviour while labelling *ours*, and ours did the opposite — it hinted, with
> a value that is not in scope there. The comment was wrong the day it was written.

`demo_exercises_every_problem_and_verdict` does this for conditions; new fixtures need the
same. The pattern to copy: a test that asserts the *exact* set of diagnostics for the
demo file, plus an `every_other_demo_file_is_free_of_<rule>` guard so the rule cannot start
firing elsewhere unnoticed.

## 5. Verify a new test fails without the fix

A green test proves nothing until you have seen it red for the right reason. Break the fix,
watch the test fail, restore. This takes thirty seconds and is the only thing separating a
regression test from a decoration.

**A break that did not apply is not a break.** If the test stays green, check the mutation is
actually in the file before you conclude anything about the test.

> Checking the control in T-178's new test meant pointing the fixture's `ansible.cfg` at a
> missing inventory. The edit's search pattern did not match, the script printed its success
> line regardless, and the test stayed green — which reads exactly like "this control does not
> work". A perfectly good control was one step from being rewritten. A `grep` of the file
> settled it in five seconds.

**Restore by reversing the edit, not by `git checkout <file>`.** That file usually also holds
the test you just wrote, and it is not committed yet.

> Undoing a deliberate break that way discarded two new tests along with it. They were
> re-typed from the same script and nothing was lost, but only because the next command
> happened to grep for them.

## 6. The editor runs a build artefact, not your source

`client/package.json` points `ansibleLsp.serverPath` at `target/release/ansible-lsp`. F5 does
not rebuild it on its own.

> A binary built the day before `T-091` landed reproduced a bug that commit had fixed, 286
> commits later. Time went into hunting a resolver defect that did not exist.

`.vscode/tasks.json` now rebuilds on launch. If you run the server any other way, build
first. When a symptom matches known-old behaviour, check the binary's timestamp before the
code.

## Ticket and commit conventions

Board mechanics — creating, closing, epics, upstream dossiers — are in
`.claude/skills/board`. Read it before touching `tasks/`.

Commits: one sentence naming what changed, and stop. Design rationale belongs in a code
comment at the site it explains, or in the ticket — not in the commit body. No
`Co-Authored-By` trailers.
