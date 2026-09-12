#!/usr/bin/env bash
# T-042 probe: how does a short module name resolve under the `collections:` keyword?
set -u
R=/tmp/t042; rm -rf $R; mkdir -p $R
CM=$R/collections/ansible_collections/ns/coll/plugins/modules
mkdir -p $CM

mod() { cat > $CM/$1.py <<PY
#!/usr/bin/python
from ansible.module_utils.basic import AnsibleModule
def main():
    AnsibleModule(argument_spec={}).exit_json(changed=False, who="$2")
main()
PY
}
mod probe_mod "ns.coll.probe_mod"
mod ping      "ns.coll.ping"          # collides with ansible.builtin.ping

# local library/ override, to place it against the collections: list
mkdir -p $R/library
cat > $R/library/ping.py <<'PY'
#!/usr/bin/python
from ansible.module_utils.basic import AnsibleModule
def main():
    AnsibleModule(argument_spec={}).exit_json(changed=False, who="local library/ ping")
main()
PY

echo "localhost ansible_connection=local" > $R/hosts

play() { cat > $R/p.yml <<YML
- hosts: all
  gather_facts: false
$1
YML
}

run() { echo "--- $1"; (cd $R && ansible-playbook -v -i hosts p.yml 2>&1 | grep -E '"who"|"ping"|ERROR|couldn.t resolve|Could not|resolve' | head -4); echo; }

play '  tasks:
    - probe_mod:'
run "A  short name, NO collections: keyword          (expect: fails)"

play '  collections: [ns.coll]
  tasks:
    - probe_mod:'
run "B  short name, collections: [ns.coll]           (expect: works -> ns.coll.probe_mod)"

play '  tasks:
    - ns.coll.probe_mod:'
run "C  FQCN, no collections:                        (expect: works)"

play '  tasks:
    - ping:'
run "D  ping, no collections:                        (baseline: which ping?)"

play '  collections: [ns.coll]
  tasks:
    - ping:'
run "E  ping, collections: [ns.coll]                 (does the list beat builtin/library?)"

play '  collections: [ns.coll]
  tasks:
    - ansible.builtin.ping:'
run "F  ansible.builtin.ping under collections:      (FQCN ignores the list?)"

rm -rf $R/library
play '  collections: [ns.coll]
  tasks:
    - ping:'
run "G  ping, collections: [ns.coll], no library/    (list vs ansible.builtin alone)"
