#!/usr/bin/env bash
# Probe B: role-reach shapes the ticket's table does not list: meta dependencies, import_role,
# collection-hosted role, include_role then a later play, include_role never executed,
# import_playbook two deep.
D=$(mktemp -d); cd "$D" || exit 1
plugin() { mkdir -p "$(dirname "$1")"; printf 'from ansible.plugins.vars import BaseVarsPlugin\nclass VarsModule(BaseVarsPlugin):\n    def get_vars(self, loader, path, entities, cache=True):\n        super(VarsModule, self).get_vars(loader, path, entities)\n        return {"%s": "yes"}\n' "$2" > "$1"; }
plugin roles/dep/vars_plugins/dv.py from_dep
plugin roles/ir/vars_plugins/iv.py from_ir
plugin roles/inc/vars_plugins/incv.py from_inc
plugin roles/never/vars_plugins/nv.py from_never
plugin sub/deeper/vars_plugins/dd.py from_deeper
plugin collections/ansible_collections/demo/vp/roles/crole/vars_plugins/cv.py from_crole
printf 'namespace: demo\nname: vp\nversion: 1.0.0\n' > collections/ansible_collections/demo/vp/galaxy.yml
mkdir -p roles/r2/meta roles/r2/tasks roles/dep/tasks roles/ir/tasks roles/inc/tasks roles/never/tasks collections/ansible_collections/demo/vp/roles/crole/tasks
printf 'dependencies: [dep]\n' > roles/r2/meta/main.yml
printf -- '- debug: msg="r2 sees dep {{ from_dep | default(\x27UNDEF\x27) }}"\n' > roles/r2/tasks/main.yml
printf -- '- debug: msg="dep own {{ from_dep | default(\x27UNDEF\x27) }}"\n' > roles/dep/tasks/main.yml
printf -- '- debug: msg="ir own {{ from_ir | default(\x27UNDEF\x27) }}"\n' > roles/ir/tasks/main.yml
printf -- '- debug: msg="inc own {{ from_inc | default(\x27UNDEF\x27) }}"\n' > roles/inc/tasks/main.yml
printf -- '- debug: msg="never own"\n' > roles/never/tasks/main.yml
printf -- '- debug: msg="crole own {{ from_crole | default(\x27UNDEF\x27) }}"\n' > collections/ansible_collections/demo/vp/roles/crole/tasks/main.yml
printf '[defaults]\ncollections_path = ./collections\n' > ansible.cfg
cat > sub/deeper/d.yml <<'Y'
- hosts: localhost
  gather_facts: false
  tasks: [{debug: {msg: "deeper {{ from_deeper | default('UNDEF') }}"}}]
Y
printf -- '- import_playbook: deeper/d.yml\n' > sub/sub.yml
cat > play.yml <<'Y'
- hosts: localhost
  gather_facts: false
  tasks:
    - debug: msg="play1 dep={{ from_dep | default('UNDEF') }} ir={{ from_ir | default('UNDEF') }} inc={{ from_inc | default('UNDEF') }} never={{ from_never | default('UNDEF') }} deeper={{ from_deeper | default('UNDEF') }} crole={{ from_crole | default('UNDEF') }}"
- import_playbook: sub/sub.yml
- hosts: localhost
  gather_facts: false
  roles: [r2, demo.vp.crole]
- hosts: localhost
  gather_facts: false
  tasks:
    - import_role: {name: ir}
    - include_role: {name: inc}
    - debug: msg="same play after include inc={{ from_inc | default('UNDEF') }}"
    - include_role: {name: never}
      when: false
- hosts: localhost
  gather_facts: false
  tasks:
    - debug: msg="play5 inc={{ from_inc | default('UNDEF') }} never={{ from_never | default('UNDEF') }}"
Y
ansible-playbook -i localhost, play.yml 2>&1 | grep -E '"msg"|ERROR|WARNING' | sed 's/^ *//'
