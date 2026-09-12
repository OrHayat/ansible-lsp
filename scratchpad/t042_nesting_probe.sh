#!/usr/bin/env bash
# T-042 probe, round 5: how nested `collections:` lists combine, and roles/ vs a listed
# collection for the same short role name.
set -u
R=/tmp/t042n; rm -rf $R; mkdir -p $R
mk() { local d=$R/collections/ansible_collections/$1/$2/plugins/modules; mkdir -p $d
  cat > $d/$3.py <<PY
#!/usr/bin/python
from ansible.module_utils.basic import AnsibleModule
def main(): AnsibleModule(argument_spec={}).exit_json(changed=False, who="$4")
main()
PY
}
mk ns a dup "ns.a.dup"; mk ns b dup "ns.b.dup"; mk ns c dup "ns.c.dup"
# same short role name in a collection and in roles/
mkdir -p $R/collections/ansible_collections/ns/a/roles/setup/tasks $R/roles/setup/tasks $R/roles/r/tasks $R/roles/r/meta
echo '- debug: {msg: "COLLECTION ns.a.setup"}' > $R/collections/ansible_collections/ns/a/roles/setup/tasks/main.yml
echo '- debug: {msg: "LOCAL roles/setup"}'     > $R/roles/setup/tasks/main.yml
echo "localhost ansible_connection=local" > $R/hosts
run() { echo "--- $1"; (cd $R && ansible-playbook -v -i hosts p.yml 2>&1 | grep -E '"who"|"msg"|couldn.t resolve|not found' | head -2); echo; }

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  tasks:
    - dup:
      collections: [ns.b]
YML
run "AA play [ns.a] + task [ns.b], both ship dup      (which wins?)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  tasks:
    - block:
        - dup:
      collections: [ns.b]
YML
run "AB play [ns.a] + block [ns.b]"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.c]
  tasks:
    - dup:
      collections: [ns.b]
YML
run "AC play [ns.c] + task [ns.b] (c also ships dup)  (confirms AA was a real collision)"

# role meta list vs a task-level list inside that role
echo 'collections: [ns.a]' > $R/roles/r/meta/main.yml
printf -- '- dup:\n  collections: [ns.b]\n' > $R/roles/r/tasks/main.yml
cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  roles: [r]
YML
run "AD role meta [ns.a] + task-in-role [ns.b]"

printf -- '- dup:\n' > $R/roles/r/tasks/main.yml
run "AE role meta [ns.a] only, task has none          (control for AD)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  tasks: [{include_role: {name: setup}}]
YML
run "AF include_role 'setup': roles/setup AND ns.a.setup both exist"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  roles: [setup]
YML
run "AG roles: [setup], same collision                (does roles: use the list too?)"
