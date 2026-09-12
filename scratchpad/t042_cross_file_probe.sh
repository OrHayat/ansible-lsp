#!/usr/bin/env bash
# T-042 probe, round 7: does a play's `collections:` reach a file it include_tasks/imports?
set -u
R=/tmp/t042x; rm -rf $R; mkdir -p $R
d=$R/collections/ansible_collections/ns/a/plugins/modules; mkdir -p $d
cat > $d/only_a.py <<'PY'
#!/usr/bin/python
from ansible.module_utils.basic import AnsibleModule
def main(): AnsibleModule(argument_spec={}).exit_json(changed=False, who="ns.a.only_a")
main()
PY
echo "- only_a:" > $R/inc.yml
echo "localhost ansible_connection=local" > $R/hosts
run() { echo "--- $1"; (cd $R && ansible-playbook -v -i hosts p.yml 2>&1 | grep -E '"who"|couldn.t resolve' | head -1); echo; }

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  tasks: [{include_tasks: inc.yml}]
YML
run "CA include_tasks under a play list      (does the list cross the file boundary?)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  tasks: [{import_tasks: inc.yml}]
YML
run "CB import_tasks under a play list"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  tasks: [{include_tasks: inc.yml}]
YML
run "CC same, no list                        (control)"
