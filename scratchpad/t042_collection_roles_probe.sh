#!/usr/bin/env bash
# T-042 item 2: collection-hosted roles — FQCN, and short names under `collections:`.
set -u
R=/tmp/t042cr; rm -rf $R; mkdir -p $R
CR=$R/collections/ansible_collections/ns/coll/roles
mkdir -p $CR/setup/tasks $R/roles/local_only/tasks
echo '- debug: {msg: "ns.coll.setup ran"}' > $CR/setup/tasks/main.yml
echo '- debug: {msg: "roles/local_only ran"}' > $R/roles/local_only/tasks/main.yml
echo "localhost ansible_connection=local" > $R/hosts
run() { echo "--- $1"; (cd $R && ansible-playbook -i hosts p.yml 2>&1 | grep -E '"msg"|ERROR' | head -2); echo; }

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  tasks: [{include_role: {name: ns.coll.setup}}]
YML
run "Q  include_role FQCN ns.coll.setup, no collections:  (expect: works)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  roles: [ns.coll.setup]
YML
run "R  roles: [ns.coll.setup] FQCN                       (expect: works)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  tasks: [{include_role: {name: setup}}]
YML
run "S  include_role short name 'setup', no collections:  (expect: fails)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.coll]
  tasks: [{include_role: {name: setup}}]
YML
run "T  include_role short name + play collections:       (does the list apply to ROLES?)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  collections: [ns.coll]
  roles: [local_only]
YML
run "U  roles: [local_only] under a list, only in roles/  (does roles/ still win/work?)"
