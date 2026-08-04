# Role-local action plugin. Pre-collections roles may ship an action_plugins/ dir beside
# their tasks/. For any `deploy_report:` task, Ansible runs THIS on the CONTROLLER instead
# of shipping demo/library/deploy_report.py to the target.
#
# T-073: hovering the task inside this role should read "runs on the controller (action
# plugin)" and link this file. Today the twin check only looks in the winning module's own
# tree (demo/library/), so it misses this and wrongly reports "runs on the target host".
from __future__ import annotations

from ansible.plugins.action import ActionBase


class ActionModule(ActionBase):
    TRANSFERS_FILES = False

    def run(self, tmp=None, task_vars=None):
        result = super().run(tmp, task_vars)
        del tmp  # legacy, unused
        # Controller-side bookkeeping, then delegate the real work to the target module.
        module_return = self._execute_module(
            module_name="deploy_report",
            module_args=self._task.args,
            task_vars=task_vars,
        )
        result.update(module_return)
        result["controller_note"] = "recorded by role-local action plugin"
        return result
