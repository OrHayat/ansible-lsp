#!/usr/bin/env bash
# T-042: what actually happens to a task file nothing includes, and to the shapes where a
# caller exists but a static index cannot link it.
set -u
R=/tmp/t042orph; rm -rf $R; mkdir -p $R
d=$R/collections/ansible_collections/ns/a/plugins/modules; mkdir -p $d
cat > $d/only_a.py <<'PY'
#!/usr/bin/python
from ansible.module_utils.basic import AnsibleModule
def main(): AnsibleModule(argument_spec={}).exit_json(changed=False, who="ns.a.only_a")
main()
PY
echo "localhost ansible_connection=local" > $R/hosts
printf -- '- only_a:\n' > $R/inc.yml
run() { echo "--- $1"; (cd $R && eval "$2" 2>&1 | grep -E '"who"|couldn.t resolve|ERROR|playbook must|FIRST' | head -2); echo; }

# 1. nothing includes inc.yml at all
printf -- '- hosts: all\n  gather_facts: false\n  collections: [ns.a]\n  tasks: [{debug: {msg: FIRST}}]\n' > $R/p.yml
run "orphan: inc.yml exists, nothing includes it" "ansible-playbook -i hosts p.yml"

# 2. run the task file directly as a playbook
run "inc.yml run directly as a playbook" "ansible-playbook -i hosts inc.yml"

# 3. included, but the path is templated — a static index cannot follow it
printf -- '- hosts: all\n  gather_facts: false\n  collections: [ns.a]\n  vars: {f: inc.yml}\n  tasks: [{include_tasks: "{{ f }}"}]\n' > $R/p.yml
run "included via include_tasks: '{{ f }}'" "ansible-playbook -v -i hosts p.yml"

# 4. included from a playbook OUTSIDE the tree (the workspace-boundary case)
mkdir -p /tmp/t042outside
printf -- "- hosts: all\n  gather_facts: false\n  collections: [ns.a]\n  tasks: [{include_tasks: $R/inc.yml}]\n" > /tmp/t042outside/outer.yml
run "included from a playbook outside the tree" "ansible-playbook -v -i hosts /tmp/t042outside/outer.yml"
