# Project-level action plugin, found via `action_plugins = ./plugins/action` in
# demo/ansible.cfg. Ansible runs it on the CONTROLLER for any `stage_files:` task,
# shadowing the same-named module in demo/library/ (which would ship to the target).
#
# T-073: the hover must consult this cfg-declared dir, report "runs on the controller", and
# link THIS file — today it only checks the winning module's own tree and misses it.
from __future__ import annotations

from ansible.plugins.action import ActionBase


class ActionModule(ActionBase):
    def run(self, tmp=None, task_vars=None):
        result = super().run(tmp, task_vars)
        del tmp
        # Resolve artifact paths on the controller, then delegate to the target module.
        result.update(
            self._execute_module(
                module_name="stage_files",
                module_args=self._task.args,
                task_vars=task_vars,
            )
        )
        return result
