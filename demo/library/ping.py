#!/usr/bin/python
# Workspace-local module deliberately named like the builtin `ping`. It pins that legacy
# `library/` dirs are searched BEFORE the ansible package ("package path always gets added
# last", loader.py:497), so a bare `ping:` task resolves HERE, not into core — and the
# hover labels it `ansible.legacy`. Shaped like the real ansible.builtin.ping so it reads
# as a genuine module rather than a stub.
from __future__ import annotations

DOCUMENTATION = r"""
---
module: ping
version_added: historical
short_description: Try to connect to host, verify a usable python and return C(pong) on success
description:
  - A trivial test module that always returns V(pong) on successful contact.
  - This is NOT ICMP ping; it just verifies login and a usable Python on the remote node.
options:
  data:
    description:
      - Data to return for the RV(ping) return value.
      - If this parameter is set to V(crash), the module will cause an exception.
    type: str
    default: pong
author:
  - Demo (@demo)
"""

EXAMPLES = r"""
- name: Verify we can log in and run python
  ping:

- name: Induce an exception to see what happens
  ping:
    data: crash
"""

RETURN = r"""
ping:
  description: Value provided with the O(data) parameter.
  returned: success
  type: str
  sample: pong
"""

from ansible.module_utils.basic import AnsibleModule


def main():
    module = AnsibleModule(
        argument_spec=dict(data=dict(type="str", default="pong")),
        supports_check_mode=True,
    )
    if module.params["data"] == "crash":
        raise Exception("boom")
    module.exit_json(ping=module.params["data"])


if __name__ == "__main__":
    main()
