#!/usr/bin/python
# The modern-collection twin case (T-086): ../action/beacon.py has the same name, so the
# task runs on the CONTROLLER and this file is only the documentation/fallback half.
from __future__ import annotations

DOCUMENTATION = r"""
---
module: beacon
short_description: demo module intercepted by its same-name action plugin
description:
  - The collection ships plugins/action/beacon.py beside this file, so the real
    logic runs on the controller.
author:
  - Demo (@demo)
"""

from ansible.module_utils.basic import AnsibleModule


def main():
    module = AnsibleModule(argument_spec=dict(msg=dict(type="str")), supports_check_mode=True)
    module.exit_json(changed=False)


if __name__ == "__main__":
    main()
