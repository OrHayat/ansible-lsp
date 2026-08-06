# The PLATFORM action plugin. Named after the prefix before the first `_`, it runs on the
# controller for every ios_* module in this collection — ios_config, ios_facts, and the
# hundred-odd others a real cisco.ios ships. It owns the persistent network_cli connection,
# which is why the module never reaches the device as code.
#
# task_executor.py:939-947,961-962 picks it: prefix = module_name.split('_')[0]; if that
# prefix is in NETWORK_GROUP_MODULES *and* an action plugin named
# <module collection>.<prefix> exists, it becomes the handler. Both halves are required —
# see demo/charlie/plugins/action/link.py for the case where only the second holds.
from __future__ import annotations

from ansible.plugins.action import ActionBase


class ActionModule(ActionBase):
    def run(self, tmp=None, task_vars=None):
        result = super().run(tmp, task_vars)
        del tmp
        # A real one opens/reuses the device connection here and sends the module's args
        # over it, rather than shipping any code to the switch.
        result.update(
            self._execute_module(
                module_name=self._task.action,
                module_args=self._task.args,
                task_vars=task_vars,
            )
        )
        return result
