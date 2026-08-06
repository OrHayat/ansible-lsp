#!/usr/bin/python
# The second member of the ios_* family, here to show that ONE platform plugin serves the
# whole family: this module has no twin of its own either, and hovering it must name the
# same cisco/ios/plugins/action/ios.py as ios_config does. (T-072)
from __future__ import annotations

DOCUMENTATION = r"""
---
module: ios_facts
short_description: Collect facts from a Cisco IOS device
description:
  - Gathers facts over the persistent connection held by the C(cisco.ios.ios)
    action plugin.
options:
  gather_subset:
    description: Subsets of facts to collect.
    type: list
    elements: str
author:
  - Demo (@demo)
"""

EXAMPLES = r"""
- name: Gather interface facts
  cisco.ios.ios_facts:
    gather_subset:
      - interfaces
"""

from ansible.module_utils.basic import AnsibleModule


def main():
    module = AnsibleModule(
        argument_spec=dict(gather_subset=dict(type="list", elements="str")),
        supports_check_mode=True,
    )
    module.exit_json(changed=False)


if __name__ == "__main__":
    main()
