#!/usr/bin/env bash
# Probe C: (1) legacy plugin NOT in vars_plugins_enabled vs group_vars/all.yml — ticket says file wins,
# inferred from vars/plugins.py:18-23; (2) vars_plugins_enabled without host_group_vars — do
# group_vars files still load? (3) ansible-inventory --playbook-dir: which plugins' names show.
D=$(mktemp -d); cd "$D" || exit 1
mkdir -p vars_plugins group_vars roles/r/vars_plugins roles/r/tasks
cat > vars_plugins/legacy.py <<'PY'
from ansible.plugins.vars import BaseVarsPlugin
class VarsModule(BaseVarsPlugin):
    def get_vars(self, loader, path, entities, cache=True):
        super(VarsModule, self).get_vars(loader, path, entities)
        return {"v_legacy_vs_gv": "plugin", "from_legacy": "yes"}
PY
printf 'from ansible.plugins.vars import BaseVarsPlugin\nclass VarsModule(BaseVarsPlugin):\n    def get_vars(self, loader, path, entities, cache=True):\n        super(VarsModule, self).get_vars(loader, path, entities)\n        return {"from_role": "yes"}\n' > roles/r/vars_plugins/rv.py
printf -- '- debug: msg=x\n' > roles/r/tasks/main.yml
printf 'v_legacy_vs_gv: group_vars_all\nv_only_in_gv: group_vars_all\n' > group_vars/all.yml
printf '[web]\nlocalhost ansible_connection=local\n' > hosts.ini
cat > play.yml <<'Y'
- hosts: localhost
  gather_facts: false
  roles: [r]
  tasks:
    - debug: msg="{{ item }}={{ lookup('vars', item, default='UNDEFINED') }}"
      loop: [v_legacy_vs_gv, v_only_in_gv, from_legacy]
Y
run() { echo "=== $1"; shift; env "$@" ansible-playbook -i hosts.ini play.yml 2>&1 | grep -oE '"msg": "[^"]*"' | sed 's/"msg": //' | tr '\n' ' '; echo; }
run "default enabled list (host_group_vars only); legacy auto-loaded"     ANSIBLE_VARS_ENABLED=host_group_vars
run "enabled = host_group_vars,legacy"                                     ANSIBLE_VARS_ENABLED=host_group_vars,legacy
run "enabled = legacy,host_group_vars"                                     ANSIBLE_VARS_ENABLED=legacy,host_group_vars
run "enabled = legacy ONLY (host_group_vars omitted)"                      ANSIBLE_VARS_ENABLED=legacy
run "enabled = '' (empty)"                                                 ANSIBLE_VARS_ENABLED=
echo "=== ansible-inventory --list --playbook-dir . (default enabled): names present?"
ansible-inventory -i hosts.ini --playbook-dir . --list 2>/dev/null | grep -oE '"(from_legacy|from_role|v_only_in_gv)"' | sort -u | tr '\n' ' '; echo
echo "=== ansible-inventory --list WITHOUT --playbook-dir:"
ansible-inventory -i hosts.ini --list 2>/dev/null | grep -oE '"(from_legacy|from_role|v_only_in_gv)"' | sort -u | tr '\n' ' '; echo
