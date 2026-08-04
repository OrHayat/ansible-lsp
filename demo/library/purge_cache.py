#!/usr/bin/python
# A plain workspace module with NO action plugin anywhere — no same-name file in a role's
# action_plugins/, no cfg action_plugins dir, no install twin. So it runs on the TARGET
# host, and the hover says exactly that. This is the contrast case: it makes the two
# controller-side cases (stage_files, deploy_report) legible by showing what "no twin"
# looks like.
from __future__ import annotations

DOCUMENTATION = r"""
---
module: purge_cache
version_added: "1.0.0"
short_description: Delete a cache directory on the target host
description:
  - Removes a cache directory on the managed node.
options:
  path:
    description: Directory to purge.
    type: path
    required: true
author:
  - Demo (@demo)
"""

EXAMPLES = r"""
- name: Clear the build cache
  purge_cache:
    path: /var/cache/build
"""

RETURN = r"""
path:
  description: The directory that was purged.
  returned: success
  type: str
  sample: /var/cache/build
"""

from ansible.module_utils.basic import AnsibleModule


def main():
    module = AnsibleModule(
        argument_spec=dict(path=dict(type="path", required=True)),
        supports_check_mode=True,
    )
    module.exit_json(changed=True, path=module.params["path"])


if __name__ == "__main__":
    main()
