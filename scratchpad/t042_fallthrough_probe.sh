#!/usr/bin/env bash
# T-042 probe, round 4: what the collections list falls through TO, and the 2-part error.
set -u
R=/tmp/t042f; rm -rf $R; mkdir -p $R/library
d=$R/collections/ansible_collections/ns/a/plugins/modules; mkdir -p $d
cat > $d/other.py <<'PY'
#!/usr/bin/python
from ansible.module_utils.basic import AnsibleModule
def main(): AnsibleModule(argument_spec={}).exit_json(changed=False, who="ns.a.other")
main()
PY
cat > $R/library/ping.py <<'PY'
#!/usr/bin/python
from ansible.module_utils.basic import AnsibleModule
def main(): AnsibleModule(argument_spec={}).exit_json(changed=False, who="local library/ ping")
main()
PY
echo "localhost ansible_connection=local" > $R/hosts
run() { echo "--- $1"; (cd $R && ansible-playbook -v -i hosts p.yml 2>&1 | grep -E '"who"|"ping"|ERROR|Cannot resolve' | head -2); echo; }

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  tasks: [{ping: }]
YML
run "V  ping: under collections: [ns.a] (no ping there), library/ping.py present"

rm -f $R/library/ping.py
run "W  same, library/ removed                            (falls to ansible.builtin?)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  tasks: [{builtin.debug: {msg: x}}]
YML
run "X  2-part name builtin.debug:                        (exact error text)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  tasks: [{a.other: }]
YML
run "Y  2-part name a.other: under collections: [ns.a]    (still invalid?)"
