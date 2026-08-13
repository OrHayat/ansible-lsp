# T-176 — Run a dynamic inventory on explicit user command

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P3       | L    | —          |

## Problem

A workspace whose inventory is `plugin: amazon.aws.aws_ec2` gets no hosts and no variables
from us. T-062 box 5 made that honest — the skip is recorded and shown — but honest is not
the same as useful. The hosts exist; we just decline to look.

The refusal itself is right and stays. Every plugin in `INVENTORY_ENABLED`'s reach is loaded
through `auto`, which means running one is running arbitrary collection code with whatever
ambient credentials the editor inherited — `AWS_PROFILE`, GCP ADC, a kubeconfig, a NetBox
token. That must never happen because a file was opened.

What is available is the other half: **run it because the user asked.** One press, cached
after.

## Approach

A command (palette + a button in the inventory panel) that shells out to
`ansible-inventory -i <source> --list`, parses the JSON, and feeds the hosts and their
variables into the index like any other source.

Not the plugin API — the CLI. Ansible already owns plugin loading, credential resolution and
its own result cache (`Cacheable`: `cache_plugin`, `cache_timeout`, `cache_connection`), so a
plugin config with `cache: true` may not even hit the network. Reimplementing any of that
would be a second, worse copy.

Three things decide whether this is worth having, and each needs measuring rather than
assuming:

- **Where the result lives.** `ScanCache` is scoped to one pass — "created by the workspace
  scan, dropped when it ends" — so nothing there survives to hold this. It needs storage that
  outlives a scan, which is new.
- **When it goes stale.** Instances come and go with no file edit to invalidate against. A
  cached host list that has quietly aged is how `hostvars['web05']` gets flagged as unknown
  for a machine that was created this morning. Staleness has to be visible, and probably
  timestamped in the panel next to the refresh button.
- **What it unblocks.** T-062 box 8 (`hostvars['name']` is an ERROR) must stay silent under a
  declined inventory. With a successful run cached, that silence can lift for the hosts the
  run reported — which is the actual payoff and the reason this is worth doing at all.

## Not in this pass

Running anything automatically, on save, on open, or on a timer. The trigger is a press,
always. If that ever changes it is a different ticket with a different argument.

## Done when

- [ ] a command runs `ansible-inventory --list` for the declined sources and nothing else runs it
- [ ] the parsed hosts and their variables reach the index, pinned by a test against recorded
      real output rather than a hand-written fixture
- [ ] the result outlives a scan, and its age is shown wherever it is used
- [ ] a failed or refused run leaves the declined state exactly as it is today — no partial
      host list, no silent success
- [ ] the panel and status bar stop warning for a source that has a fresh successful run
- [ ] nothing executes without a user gesture, pinned by a test
