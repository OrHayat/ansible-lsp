#!/bin/sh
# T-062: a DYNAMIC inventory in its OTHER form — an executable script.
#
# SILENCED: pick this and no variables appear, and the panel says why.
#
# Unlike `inventory-dynamic.yml` next door, this one you can actually check. It needs no
# credentials, no network and no cloud account, so the claim below is testable rather than
# taken on trust:
#
#     ansible-inventory -i demo/inventory-dynamic.sh --list
#
# That prints mock01 and its variables — proof this is a genuine dynamic inventory and that
# Ansible really does run it. Our tool reads the same file and reports nothing, because
# producing that output means EXECUTING this file, and an editor must not run a repo's code
# to answer a hover. What we do instead is say so: the inventory panel and the status bar
# both name this file as detected-but-not-executed.
#
# That distinction is the point. `mock_dynamic_var` below is not "undefined" — it is
# unknown, and those are different claims. A tool that cannot tell them apart reports the
# hosts it never looked for as hosts that do not exist.
#
# The execute bit is load-bearing and so is the `#!` line: a data file that merely carries a
# stray +x is NOT dynamic — Ansible runs it, the script plugin fails, and the ini plugin
# reads it anyway. Measured, both ways.
cat <<'JSON'
{
  "_meta": {
    "hostvars": {
      "mock01": {
        "mock_dynamic_var": "FROM_A_SCRIPT_WE_NEVER_RUN",
        "mock_region": "nowhere-1"
      }
    }
  },
  "all": { "children": ["mock_group"] },
  "mock_group": { "hosts": ["mock01"] }
}
JSON
