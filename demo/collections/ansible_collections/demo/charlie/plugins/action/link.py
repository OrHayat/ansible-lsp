# A decoy, and the whole point of the link_status fixture: this file exists and is named
# after that module's prefix, exactly like cisco/ios/plugins/action/ios.py is. Ansible still
# ignores it for `demo.charlie.link_status`, because `link` is not in NETWORK_GROUP_MODULES
# and the platform lookup requires BOTH conditions.
#
# It is reachable the ordinary way — a task written `demo.charlie.link:` binds to it as a
# same-name plugin. It is only the *platform* fallback that must not fire. (T-072)
from __future__ import annotations

from ansible.plugins.action import ActionBase


class ActionModule(ActionBase):
    def run(self, tmp=None, task_vars=None):
        result = super().run(tmp, task_vars)
        del tmp
        result["msg"] = "ran on the controller, as a same-name plugin — not as a platform"
        return result
