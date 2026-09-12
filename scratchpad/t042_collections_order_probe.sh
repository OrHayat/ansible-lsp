#!/usr/bin/env bash
# T-042 probe, round 2: list order, fall-through, and which entities carry `collections:`.
set -u
R=/tmp/t042o; rm -rf $R; mkdir -p $R
mk() { # mk <ns> <coll> <module> <who>
  local d=$R/collections/ansible_collections/$1/$2/plugins/modules; mkdir -p $d
  cat > $d/$3.py <<PY
#!/usr/bin/python
from ansible.module_utils.basic import AnsibleModule
def main(): AnsibleModule(argument_spec={}).exit_json(changed=False, who="$4")
main()
PY
}
mk ns a dup   "ns.a.dup"
mk ns b dup   "ns.b.dup"
mk ns b only_b "ns.b.only_b"
echo "localhost ansible_connection=local" > $R/hosts
run() { echo "--- $1"; (cd $R && ansible-playbook -v -i hosts p.yml 2>&1 | grep -E '"who"|"msg"|couldn.t resolve|ERROR' | head -2); echo; }

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a, ns.b]
  tasks: [{dup: }]
YML
run "K1 collections: [ns.a, ns.b], both have dup   (expect ns.a — first wins)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.b, ns.a]
  tasks: [{dup: }]
YML
run "K2 reversed order                            (expect ns.b — confirms order matters)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  tasks: [{debug: {msg: fellthrough}}]
YML
run "L  debug: under collections: [ns.a] (no debug there) (expect builtin still reached)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  tasks:
    - only_b:
      collections: [ns.b]
YML
run "M  TASK-level collections:                   (does a task carry it?)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  tasks:
    - block:
        - only_b:
      collections: [ns.b]
YML
run "N  BLOCK-level collections:                  (does a block carry it?)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  tasks: [{ns.b.dup: }]
YML
run "O  FQCN of a collection NOT in the list      (expect ns.b — FQCN ignores the list)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.a]
  tasks: [{only_b: }]
YML
run "P  short name in a collection NOT listed     (expect: fails)"
