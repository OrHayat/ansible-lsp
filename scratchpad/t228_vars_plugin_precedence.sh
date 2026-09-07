#!/usr/bin/env bash
# T-228: where a vars plugin's output lands against every YAML source, and who wins a tie.
# The plugin answers differently per entity so a same-rung tie is distinguishable from a
# later-rung win. Expected on ansible-core 2.21.3 (default VARIABLE_PRECEDENCE):
#   order prec,hgv : all-only vs group_vars/all.yml -> file;  web vs group_vars/web.yml -> file;
#                    host vs host_vars/localhost.yml -> file
#   order hgv,prec : the same three -> plugin           (same rung: last in the list wins)
#   both orders    : v_both -> plugin-host (host rung beats group rung);
#                    v_hostonly_vs_inv -> plugin-host (inline inventory var merges first);
#                    v_any_vs_default -> plugin; v_any_vs_rolevars -> role_vars; v_any_vs_play -> play_vars
#   control        : plugin not enabled -> every YAML/inventory value, v_both undefined
PB=${ANSIBLE_PLAYBOOK:-ansible-playbook}
D=$(mktemp -d); cd "$D" || exit 1
mkdir -p vars_plugins group_vars host_vars roles/r/defaults roles/r/vars
cat > vars_plugins/prec.py <<'PY'
from ansible.plugins.vars import BaseVarsPlugin
from ansible.inventory.host import Host
class VarsModule(BaseVarsPlugin):
    REQUIRES_ENABLED = True
    def get_vars(self, loader, path, entities, cache=True):
        super(VarsModule, self).get_vars(loader, path, entities)
        out = {}
        for e in entities:
            kind = "host" if isinstance(e, Host) else "group"
            for k in ("v_both", "v_hostonly_vs_inv", "v_any_vs_default", "v_any_vs_rolevars", "v_any_vs_play"):
                out[k] = "plugin-" + kind
            if kind == "group" and e.name == "all":
                out["v_all_vs_gv_all"] = "plugin-group"
            if kind == "group" and e.name == "web":
                out["v_web_vs_gv_web"] = "plugin-group"
            if kind == "host":
                out["v_host_vs_hv"] = "plugin-host"
        return out
PY
printf 'v_all_vs_gv_all: group_vars_all\n' > group_vars/all.yml
printf 'v_web_vs_gv_web: group_vars_web\n' > group_vars/web.yml
printf 'v_host_vs_hv: host_vars\n' > host_vars/localhost.yml
printf '[web]\nlocalhost ansible_connection=local v_hostonly_vs_inv=inventory\n' > hosts.ini
printf 'v_any_vs_default: role_default\n' > roles/r/defaults/main.yml
printf 'v_any_vs_rolevars: role_vars\n' > roles/r/vars/main.yml
cat > play.yml <<'Y'
- hosts: localhost
  gather_facts: false
  vars: {v_any_vs_play: play_vars}
  roles: [r]
  tasks:
    - debug: msg="{{ item }}={{ lookup('vars', item, default='UNDEFINED') }}"
      loop: [v_both, v_all_vs_gv_all, v_web_vs_gv_web, v_host_vs_hv, v_hostonly_vs_inv, v_any_vs_default, v_any_vs_rolevars, v_any_vs_play]
Y
run() { echo "=== $1"; shift; env "$@" $PB -i hosts.ini play.yml 2>&1 | grep -oE '"msg": "[^"]*"' | sed 's/"msg": //' | tr '\n' ' '; echo; }
run "order prec,host_group_vars"  ANSIBLE_VARS_ENABLED=prec,host_group_vars
run "order host_group_vars,prec"  ANSIBLE_VARS_ENABLED=host_group_vars,prec
run "control: plugin not enabled" ANSIBLE_VARS_ENABLED=host_group_vars
