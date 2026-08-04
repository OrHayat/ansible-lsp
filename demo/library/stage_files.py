#!/usr/bin/python
# Workspace-local module — runs on the TARGET host. A same-named action plugin lives in the
# cfg-declared dir ./plugins/action (see `action_plugins` in demo/ansible.cfg), which
# Ansible runs on the CONTROLLER instead.
# T-073: the hover must consult that cfg dir, say "runs on the controller", and link it.
from __future__ import annotations

DOCUMENTATION = r"""
---
module: stage_files
version_added: "1.0.0"
short_description: Copy release artifacts into a staging dir on the target
description:
  - Places build artifacts under a staging directory on the managed node.
options:
  dest:
    description: Staging directory on the target.
    type: path
    required: true
author:
  - Demo (@demo)
"""

EXAMPLES = r"""
- name: Stage the release
  stage_files:
    dest: /srv/stage
"""

RETURN = r"""
dest:
  description: The staging directory used.
  returned: success
  type: str
  sample: /srv/stage
"""

from ansible.module_utils.basic import AnsibleModule


def main():
    module = AnsibleModule(
        argument_spec=dict(dest=dict(type="path", required=True)),
        supports_check_mode=True,
    )
    module.exit_json(changed=False, dest=module.params["dest"])


if __name__ == "__main__":
    main()
