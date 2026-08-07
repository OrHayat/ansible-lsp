# The same-name twin of ../modules/beacon.py, in the modern collection layout. Its
# existence means `demo.charlie.beacon:` runs on the controller, and the hover must say so
# and link this file — on every platform, including Windows (T-086).
from __future__ import annotations

from ansible.plugins.action import ActionBase


class ActionModule(ActionBase):
    def run(self, tmp=None, task_vars=None):
        result = super().run(tmp, task_vars)
        del tmp
        result["msg"] = "ran on the controller, as the module's same-name collection twin"
        return result
