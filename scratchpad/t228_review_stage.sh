#!/usr/bin/env bash
# Probe A: does `stage` / RUN_VARS_PLUGINS change *whether a name exists by task time*, per location,
# and does a stage:inventory plugin land on the rung the ticket's precedence table says?
# Each plugin has its own env var for `stage` so rows can be varied independently.
D=$(mktemp -d); cd "$D" || exit 1
plugin() { # plugin <path> <name> <var> <envname> [extra class body]
  mkdir -p "$(dirname "$1")"
  cat > "$1" <<PY
from ansible.plugins.vars import BaseVarsPlugin
DOCUMENTATION = '''
    name: $2
    short_description: probe
    options:
      stage:
        type: str
        choices: ['all', 'task', 'inventory']
        env:
          - name: $4
'''
class VarsModule(BaseVarsPlugin):
$5
    def get_vars(self, loader, path, entities, cache=True):
        super(VarsModule, self).get_vars(loader, path, entities)
        return {"$3": "yes"}
PY
}
plugin vars_plugins/pbstg.py pbstg from_pb STG_PB ""
plugin roles/r/vars_plugins/rolestg.py rolestg from_role STG_ROLE ""
plugin sub/vars_plugins/substg.py substg from_sub STG_SUB ""
mkdir -p roles/r/tasks; printf -- '- debug: msg="in role {{ from_role | default(\x27UNDEF\x27) }}"\n' > roles/r/tasks/main.yml
cat > sub/sub.yml <<'Y'
- hosts: localhost
  gather_facts: false
  tasks: [{debug: {msg: "in sub {{ from_sub | default('UNDEF') }}"}}]
Y
cat > play.yml <<'Y'
- hosts: localhost
  gather_facts: false
  tasks:
    - debug: msg="pb={{ from_pb | default('UNDEF') }} role={{ from_role | default('UNDEF') }} sub={{ from_sub | default('UNDEF') }}"
- import_playbook: sub/sub.yml
- hosts: localhost
  gather_facts: false
  roles: [r]
Y
run() { echo "=== $1"; shift; env "$@" ansible-playbook -i localhost, play.yml 2>&1 | grep -oE '"msg": "[^"]*"' | tr '\n' ' '; echo; }
run "no stage set, default run_vars_plugins=demand (all three expected yes)"
run "all three stage=task (control: option plumbing, expect yes yes yes)"          STG_PB=task STG_ROLE=task STG_SUB=task
run "all three stage=inventory"                                                    STG_PB=inventory STG_ROLE=inventory STG_SUB=inventory
run "all three stage=all"                                                          STG_PB=all STG_ROLE=all STG_SUB=all
run "no stage set, ANSIBLE_RUN_VARS_PLUGINS=start"                                 ANSIBLE_RUN_VARS_PLUGINS=start
run "no stage set, ANSIBLE_RUN_VARS_PLUGINS=start, --playbook-dir?? n/a; with -i pointing at a dir beside roles" ANSIBLE_RUN_VARS_PLUGINS=start

echo; echo "##### precedence with stage=inventory (plugin listed AFTER host_group_vars) #####"
D2=$(mktemp -d); cd "$D2" || exit 1
mkdir -p vars_plugins group_vars host_vars
cat > vars_plugins/pstg.py <<'PY'
from ansible.plugins.vars import BaseVarsPlugin
from ansible.inventory.host import Host
DOCUMENTATION = '''
    name: pstg
    short_description: probe
    options:
      stage:
        type: str
        choices: ['all', 'task', 'inventory']
        env:
          - name: STG_P
'''
class VarsModule(BaseVarsPlugin):
    REQUIRES_ENABLED = True
    def get_vars(self, loader, path, entities, cache=True):
        super(VarsModule, self).get_vars(loader, path, entities)
        out = {}
        for e in entities:
            if isinstance(e, Host):
                out["v_host_vs_hv"] = "plugin-host"
                out["v_host_vs_inline_host"] = "plugin-host"
            elif e.name == "web":
                out["v_web_vs_gv_web"] = "plugin-group"
            elif e.name == "all":
                out["v_all_vs_inline_web"] = "plugin-all"
        return out
PY
printf 'v_web_vs_gv_web: group_vars_web\n' > group_vars/web.yml
printf 'v_host_vs_hv: host_vars\n' > host_vars/localhost.yml
printf '[web]\nlocalhost ansible_connection=local v_host_vs_inline_host=inline_host\n[web:vars]\nv_all_vs_inline_web=inline_web\n' > hosts.ini
cat > play.yml <<'Y'
- hosts: localhost
  gather_facts: false
  tasks:
    - debug: msg="{{ item }}={{ lookup('vars', item, default='UNDEFINED') }}"
      loop: [v_web_vs_gv_web, v_host_vs_hv, v_host_vs_inline_host, v_all_vs_inline_web]
Y
run2() { echo "=== $1"; shift; env "$@" ansible-playbook -i hosts.ini play.yml 2>&1 | grep -oE '"msg": "[^"]*"' | sed 's/"msg": //' | tr '\n' ' '; echo; }
run2 "stage=task, order hgv,pstg (ticket row: plugin wins files)"  ANSIBLE_VARS_ENABLED=host_group_vars,pstg STG_P=task
run2 "stage=inventory, order hgv,pstg"                              ANSIBLE_VARS_ENABLED=host_group_vars,pstg STG_P=inventory
run2 "stage=all, order hgv,pstg"                                    ANSIBLE_VARS_ENABLED=host_group_vars,pstg STG_P=all
run2 "control: not enabled"                                         ANSIBLE_VARS_ENABLED=host_group_vars STG_P=inventory
