#!/usr/bin/env bash
# T-042 probe, round 6: is an inner `collections:` a merge with the outer one, or a replace?
set -u
R=/tmp/t042m; rm -rf $R; mkdir -p $R
mk() { local d=$R/collections/ansible_collections/$1/$2/plugins/modules; mkdir -p $d
  cat > $d/$3.py <<PY
#!/usr/bin/python
from ansible.module_utils.basic import AnsibleModule
def main(): AnsibleModule(argument_spec={}).exit_json(changed=False, who="$4")
main()
PY
}
mk ns a only_a "ns.a.only_a"
mk ns b only_b "ns.b.only_b"
mkdir -p $R/roles/r/tasks $R/roles/r/meta
echo "localhost ansible_connection=local" > $R/hosts
run() { echo "--- $1"; (cd $R && ansible-playbook -v -i hosts p.yml 2>&1 | grep -E '"who"|couldn.t resolve' | head -1); echo; }

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  tasks:
    - only_a:
      collections: [ns.b]
YML
run "BA play [ns.a], task [ns.b], module only in ns.a   (merge -> works; replace -> fails)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  tasks:
    - only_b:
      collections: [ns.b]
YML
run "BB same shape, module only in ns.b                 (control: the inner list is live)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  tasks:
    - block:
        - only_a:
      collections: [ns.b]
YML
run "BC play [ns.a], block [ns.b], module only in ns.a"

echo 'collections: [ns.a]' > $R/roles/r/meta/main.yml
printf -- '- only_a:\n  collections: [ns.b]\n' > $R/roles/r/tasks/main.yml
cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  roles: [r]
YML
run "BD role meta [ns.a], task-in-role [ns.b], module only in ns.a"
