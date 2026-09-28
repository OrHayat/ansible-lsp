# T-241 — Differential precedence test against ansible-core's VariableManager as an oracle

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-112 | —          |

## Problem

Whenever the tool picks "the definition that applies here", it claims to know Ansible's
precedence: `vars::effective` (`vars.rs:964`), and the hover and go-to-definition orderings that
use it (`main.rs:2930`, `main.rs:3069`). Every rule behind `VarSource::precedence` was measured by
hand, one case at a time: role params beat entry `vars:`, same-precedence ties go to the later
file, and so on. Nothing re-checks them as a set. A new source or a changed rule can quietly
break an old one.

ansible-core already answers the question. `VariableManager.get_vars` gives the resolved
variables for a host, play and task. It is no use as our engine: it executes dynamic inventory
and vars plugins, needs everything installed, stops at the first broken file, and returns only
the winning value, with no file, span or other candidates. But as a **test oracle** it turns
"precedence bugs nobody has noticed yet" into a list that a test produces.

## Measured (ansible-core 2.21.3)

A proof of concept ran: a fixture with variables `a`–`g`, each defined at several levels with a
different literal per level (group_vars, host_vars, role defaults, role vars, play vars, role
param, block vars, task vars). It was dumped through `VariableManager` and compared with a real
`ansible-playbook` run of the same fixture. They agreed on every variable, for both tasks.

What that showed about the API:

- `get_vars(play=, host=)`, with no task, includes group_vars, host_vars, play vars, **role
  defaults and role vars** of the play's roles. It does **not** include role params or
  block/task `vars:`. Those only appear with `task=`: the role task saw `e: role_param`, and the
  play task saw `f: task_vars` over `block_vars` and `g: block_vars`.
- So the oracle must query **per task**, not just per play. The levels that are only visible
  per task are where the known incidents were (CLAUDE.md rule 1).
- `Playbook.load` needs `context.CLIARGS` set first (`tags`, `skip_tags`); tasks come from
  `play.compile()`.

## Approach

- A test-only Python helper, outside the server binary. Given a fixture (inventory + playbook),
  for each host and each chosen task, it dumps `get_vars(play, host, task)` as JSON.
- A Rust test that runs it and, for each (host, task, name), asserts that the literal value of
  our `effective` definition at that task's position equals the oracle's.
- Fixtures are built so the probe can fail (rule 2): every name is defined at two or more levels
  with a different literal at each. Only literal values are compared. Templated values and
  anything computed at runtime (`set_fact`, `register`, `include_vars`, facts) are excluded, not
  faked.
- Fixtures are synthetic and live in the repo. A private corpus may be run through the helper
  locally to hunt bugs, but nothing from it is committed.
- `VariableManager` is internal, not a stable API. Pin the ansible-core version the oracle ran
  against and record it in the output. `install.rs` already detects the installed core.
- The test must not pass silently when Python or ansible-core is missing. Make it `#[ignore]`d
  and run on purpose (`--ignored`), with a hard failure inside it when the interpreter is
  absent. Never skip-as-pass.

## Done when

- [ ] the helper dumps per-host, per-task resolved literals for a fixture, with the core version
- [ ] a fixture covers every `VarSource` we rank, each name colliding across at least two levels
- [ ] the differential test compares `effective` against the oracle for every (host, task,
      name), and a deliberately wrong `precedence()` makes it fail (rule 5)
- [ ] a missing interpreter or core fails loudly; it never reads as green
- [ ] each mismatch found is filed as a bug with the fixture as its reproduction, or fixed
