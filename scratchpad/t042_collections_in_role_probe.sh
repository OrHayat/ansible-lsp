#!/usr/bin/env bash
# T-042 probe: does a role's own collections: list apply to tasks inside the role,
# and does the PLAY's list leak into a role it calls?
set -u
R=/tmp/t042r; rm -rf $R; mkdir -p $R
CM=$R/collections/ansible_collections/ns/coll/plugins/modules; mkdir -p $CM
cat > $CM/probe_mod.py <<'PY'
#!/usr/bin/python
from ansible.module_utils.basic import AnsibleModule
def main(): AnsibleModule(argument_spec={}).exit_json(changed=False, who="ns.coll.probe_mod")
main()
PY
mkdir -p $R/roles/r/tasks $R/roles/r/meta
echo "- probe_mod:" > $R/roles/r/tasks/main.yml
echo "localhost ansible_connection=local" > $R/hosts

run() { echo "--- $1"; (cd $R && ansible-playbook -v -i hosts p.yml 2>&1 | grep -E '"who"|couldn.t resolve' | head -2); echo; }

printf -- '- hosts: all\n  gather_facts: false\n  roles: [r]\n' > $R/p.yml
: > $R/roles/r/meta/main.yml
run "H  role task, short name, no collections: anywhere      (expect: fails)"

printf 'collections: [ns.coll]\n' > $R/roles/r/meta/main.yml
run "I  role's OWN meta/main.yml collections:                 (expect: works)"

: > $R/roles/r/meta/main.yml
printf -- '- hosts: all\n  gather_facts: false\n  collections: [ns.coll]\n  roles: [r]\n' > $R/p.yml
run "J  PLAY's collections:, role has none                    (does it leak in?)"
