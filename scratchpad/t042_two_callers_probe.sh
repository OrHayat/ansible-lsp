#!/usr/bin/env bash
# T-042: one included file, two callers with DIFFERENT `collections:` lists.
set -u
R=/tmp/t042tc; rm -rf $R; mkdir -p $R
mk() { local d=$R/collections/ansible_collections/$1/$2/plugins/modules; mkdir -p $d
  cat > $d/$3.py <<PY
#!/usr/bin/python
from ansible.module_utils.basic import AnsibleModule
def main(): AnsibleModule(argument_spec={}).exit_json(changed=False, who="$4")
main()
PY
}
mk ns a shared_name "ns.a.shared_name"
mk ns b shared_name "ns.b.shared_name"
mk ns a only_a      "ns.a.only_a"

echo "localhost ansible_connection=local" > $R/hosts
run() { printf '%-42s ' "$1"; (cd $R && ansible-playbook -v -i hosts $2 2>&1 | grep -oE '"who": "[^"]*"|couldn.t resolve module/action .[^.]*.' | head -1); echo; }

# ONE included file, used by both playbooks
printf -- '- shared_name:\n' > $R/inc.yml
printf -- '- hosts: all\n  gather_facts: false\n  collections: [ns.a]\n  tasks: [{include_tasks: inc.yml}]\n' > $R/p1.yml
printf -- '- hosts: all\n  gather_facts: false\n  collections: [ns.b]\n  tasks: [{include_tasks: inc.yml}]\n' > $R/p2.yml
run "inc.yml via p1.yml (collections: [ns.a])" p1.yml
run "inc.yml via p2.yml (collections: [ns.b])" p2.yml

# and the case where one caller makes it work and the other makes it fail
printf -- '- only_a:\n' > $R/inc.yml
run "only_a via p1.yml (ns.a has it)" p1.yml
run "only_a via p2.yml (ns.b does not)" p2.yml
