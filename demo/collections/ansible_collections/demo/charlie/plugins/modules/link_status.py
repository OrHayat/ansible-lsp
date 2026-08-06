#!/usr/bin/python
# The negative case for T-072, and the sharp one: an action plugin named after this
# module's prefix DOES exist (../action/link.py), but `link` is not in
# NETWORK_GROUP_MODULES — so Ansible does NOT use it, and this module ships to and runs on
# the TARGET host like any other.
#
# It pins the AND in task_executor.py:961-962: `all((module_prefix in NETWORK_GROUP_MODULES,
# action_loader.has_plugin(network_action)))`. A fix that only checks whether
# plugins/action/<prefix>.py exists passes the ios_* cases and gets this one wrong.
from __future__ import annotations

DOCUMENTATION = r"""
---
module: link_status
short_description: Report link state on the target host
description:
  - Reads link state on the managed node. Not a network-device module — it runs
    on the target, despite the C(link_) prefix.
options:
  interface:
    description: Interface to inspect.
    type: str
    required: true
author:
  - Demo (@demo)
"""

EXAMPLES = r"""
- name: Check eth0
  demo.charlie.link_status:
    interface: eth0
"""

from ansible.module_utils.basic import AnsibleModule


def main():
    module = AnsibleModule(
        argument_spec=dict(interface=dict(type="str", required=True)),
        supports_check_mode=True,
    )
    module.exit_json(changed=False)


if __name__ == "__main__":
    main()
