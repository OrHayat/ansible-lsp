#!/usr/bin/python
# A network device module. There is NO plugins/action/ios_config.py — network collections
# ship one action plugin per PLATFORM, not per module, and cisco/ios/plugins/action/ios.py
# handles every ios_* task in this collection (it owns the persistent device connection).
#
# T-072: the hover's same-name twin check looks for ios_config.py in plugins/action/, finds
# nothing, and says "runs on the target host" — wrong on both counts. A switch has no Python
# to ship a module to; the code runs on the CONTROLLER, via a plugin the hover never looked
# for. The fix splits the prefix before the first `_`, checks it against
# NETWORK_GROUP_MODULES, and looks for plugins/action/<prefix>.py in this same tree.
from __future__ import annotations

DOCUMENTATION = r"""
---
module: ios_config
short_description: Manage configuration sections on a Cisco IOS device
description:
  - Sends configuration lines to an IOS device over the persistent connection
    held by the C(cisco.ios.ios) action plugin.
options:
  lines:
    description: Configuration commands to send.
    type: list
    elements: str
author:
  - Demo (@demo)
"""

EXAMPLES = r"""
- name: Set the hostname
  cisco.ios.ios_config:
    lines:
      - hostname edge-01
"""

from ansible.module_utils.basic import AnsibleModule


def main():
    module = AnsibleModule(
        argument_spec=dict(lines=dict(type="list", elements="str")),
        supports_check_mode=True,
    )
    module.exit_json(changed=False)


if __name__ == "__main__":
    main()
