#!/usr/bin/env bash
# T-042 probe, round 8: a templated entry is never rendered (`collections` is static=True).
# Does it still REPLACE the outer list?
set -u
R=/tmp/t042te; rm -rf $R; mkdir -p $R
d=$R/collections/ansible_collections/ns/a/plugins/modules; mkdir -p $d
cat > $d/only_a.py <<'PY'
#!/usr/bin/python
from ansible.module_utils.basic import AnsibleModule
def main(): AnsibleModule(argument_spec={}).exit_json(changed=False, who="ns.a.only_a")
main()
PY
echo "localhost ansible_connection=local" > $R/hosts
run() { echo "--- $1"; (cd $R && ansible-playbook -v -i hosts p.yml 2>&1 | grep -E '"who"|couldn.t resolve' | head -1); echo; }

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  vars: {c: ns.a}
  tasks:
    - only_a:
      collections: ["{{ c }}"]
YML
run "DA play [ns.a] + task ['{{ c }}'] where c=ns.a   (replace by a dead entry?)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  vars: {c: ns.a}
  tasks:
    - only_a:
YML
run "DB same without the task list                    (control)"
