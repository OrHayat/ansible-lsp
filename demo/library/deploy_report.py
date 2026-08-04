#!/usr/bin/python
# Workspace-local module in demo/library/ — ships to and runs on the TARGET host.
# It exists so a bare `deploy_report:` task resolves to a real module; the `reporting`
# role ships a controller-side action plugin of the SAME name
# (roles/reporting/action_plugins/deploy_report.py) that intercepts the task.
# T-073: the hover should surface that role-local plugin and say "runs on the controller".
from __future__ import annotations

DOCUMENTATION = r"""
---
module: deploy_report
version_added: "1.0.0"
short_description: Record the outcome of a deploy on the target host
description:
  - Appends a one-line deployment summary to a log file on the managed node.
options:
  summary:
    description: One-line summary to record.
    type: str
    required: true
  path:
    description: File the summary is appended to.
    type: path
    default: /var/log/deploy_report.log
author:
  - Demo (@demo)
"""

EXAMPLES = r"""
- name: Record a deploy result
  deploy_report:
    summary: "{{ inventory_hostname }} converged"
"""

RETURN = r"""
path:
  description: The file the summary was written to.
  returned: success
  type: str
  sample: /var/log/deploy_report.log
changed:
  description: Whether the file was modified.
  returned: always
  type: bool
  sample: true
"""

from ansible.module_utils.basic import AnsibleModule


def main():
    module = AnsibleModule(
        argument_spec=dict(
            summary=dict(type="str", required=True),
            path=dict(type="path", default="/var/log/deploy_report.log"),
        ),
        supports_check_mode=True,
    )
    path = module.params["path"]
    if not module.check_mode:
        with open(path, "a", encoding="utf-8") as fh:
            fh.write(module.params["summary"] + "\n")
    module.exit_json(changed=True, path=path)


if __name__ == "__main__":
    main()
