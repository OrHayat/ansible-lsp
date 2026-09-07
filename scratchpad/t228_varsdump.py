#!/usr/bin/env python3
# T-228 rung 4: load a playbook the way ansible-playbook does (which is what adds role-local
# plugin dirs), then ask the variable manager for each host's variables per play. No task runs;
# only the vars plugins execute, exactly as at run time. Usage: varsdump.py PLAYBOOK INVENTORY
# Run with the interpreter of the Ansible install. Prints the names starting with from_ as a
# demo filter; a real consumer takes the whole dict.
import sys
from ansible import context
from ansible.module_utils.common.collections import ImmutableDict
from ansible.parsing.dataloader import DataLoader
from ansible.inventory.manager import InventoryManager
from ansible.vars.manager import VariableManager
from ansible.playbook import Playbook

playbook, inventory = sys.argv[1], sys.argv[2]
context.CLIARGS = ImmutableDict(connection="local", module_path=[], forks=1, become=None,
    become_method=None, become_user=None, check=False, diff=False, verbosity=0, syntax=False,
    start_at_task=None, tags=("all",), skip_tags=(), extra_vars=(), vault_ids=[], vault_password_files=[],
    ask_vault_pass=False, listhosts=False, listtasks=False, listtags=False, step=False, subset=None)
loader = DataLoader()
inv = InventoryManager(loader=loader, sources=[inventory])
vm = VariableManager(loader=loader, inventory=inv)
pb = Playbook.load(playbook, variable_manager=vm, loader=loader)
for i, play in enumerate(pb.get_plays(), 1):
    for host in inv.get_hosts(pattern=play.hosts):
        v = vm.get_vars(play=play, host=host)
        found = sorted(k for k in v if k.startswith("from_"))
        print(f"play {i} host {host.name}: {found}")
