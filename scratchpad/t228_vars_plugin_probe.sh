#!/usr/bin/env bash
# T-228: which vars_plugins/ locations does Ansible load, and how far does each reach?
# One plugin per location, one variable each; the last run removes the playbook-adjacent
# dir so the probe can fail. Expected on ansible-core 2.21.3:
#   pb-yes, role-yes (even in play 1, before the role's play), gated only when enabled,
#   subpb-yes from play 1 onward, taskdir UNDEF, coll only when enabled by FQCN,
#   and the control run fails with "'from_pb_plugin' is undefined".
PB=${ANSIBLE_PLAYBOOK:-ansible-playbook}
D=$(mktemp -d)
cd "$D" || exit 1

plugin() { # plugin <path> <var> <value> [REQUIRES_ENABLED line]
  mkdir -p "$(dirname "$1")"
  printf 'from ansible.plugins.vars import BaseVarsPlugin\nclass VarsModule(BaseVarsPlugin):\n%s    def get_vars(self, loader, path, entities, cache=True):\n        super(VarsModule, self).get_vars(loader, path, entities)\n        return {"%s": "%s"}\n' "${4:-}" "$2" "$3" > "$1"
}
plugin vars_plugins/pbvars.py from_pb_plugin pb-yes
plugin roles/r/vars_plugins/rolevars.py from_role_plugin role-yes
plugin cfgdir/gated.py from_gated_plugin gated-yes "    REQUIRES_ENABLED = True
"
plugin tasks/vars_plugins/tv.py from_taskdir_plugin taskdir-yes
plugin sub/vars_plugins/sv.py from_subpb_plugin subpb-yes
plugin collections/ansible_collections/demo/vp/plugins/vars/cv.py from_coll_plugin coll-yes
printf 'namespace: demo\nname: vp\nversion: 1.0.0\n' > collections/ansible_collections/demo/vp/galaxy.yml

mkdir -p roles/r/tasks
printf -- '- debug: msg="role sees {{ from_role_plugin }}"\n' > roles/r/tasks/main.yml
printf -- '- debug: msg="taskdir {{ from_taskdir_plugin | default(\x27UNDEF-taskdir\x27) }}"\n' > tasks/t.yml
cat > sub/sub.yml <<'Y'
- hosts: localhost
  gather_facts: false
  tasks:
    - debug: msg="in sub {{ from_subpb_plugin | default('UNDEF-subpb') }}"
Y
cat > play.yml <<'Y'
- hosts: localhost
  gather_facts: false
  tasks:
    - debug: msg="play1 pb {{ from_pb_plugin }}"
    - debug: msg="play1 role {{ from_role_plugin | default('UNDEF-role') }}"
    - debug: msg="play1 gated {{ from_gated_plugin | default('UNDEF-gated') }}"
    - debug: msg="play1 sub {{ from_subpb_plugin | default('UNDEF-subpb') }}"
    - debug: msg="play1 coll {{ from_coll_plugin | default('UNDEF-coll') }}"
    - include_tasks: tasks/t.yml
- import_playbook: sub/sub.yml
- hosts: localhost
  gather_facts: false
  roles: [r]
Y
cat > play_include.yml <<'Y'
- hosts: localhost
  gather_facts: false
  tasks:
    - debug: msg="play1 role via include {{ from_role_plugin | default('UNDEF-role') }}"
- hosts: localhost
  gather_facts: false
  tasks:
    - include_role: {name: r}
Y
printf '[defaults]\nvars_plugins = ./cfgdir\ncollections_path = ./collections\n' > ansible.cfg

run() { echo "=== $1 ==="; shift; "$@" 2>&1 | grep -E '"msg"|ERROR' | sed 's/^ *//'; echo; }
run "roles: — default enabling"            $PB -i localhost, play.yml
run "gated + collection enabled"           env ANSIBLE_VARS_ENABLED=host_group_vars,gated,demo.vp.cv $PB -i localhost, play.yml
run "role reached only by include_role"    $PB -i localhost, play_include.yml
mv vars_plugins vars_plugins.off
run "CONTROL: playbook dir removed (expect undefined)" $PB -i localhost, play.yml
