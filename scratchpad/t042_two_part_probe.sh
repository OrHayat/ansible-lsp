#!/usr/bin/env bash
# T-042 item 3: which "invalid name" message fires for which task shape.
set -u
R=/tmp/t042t; rm -rf $R; mkdir -p $R
echo "localhost ansible_connection=local" > $R/hosts
run() { echo "--- $1"; (cd $R && ansible-playbook -i hosts p.yml 2>&1 | grep -E "Cannot resolve|couldn't resolve|no module/action" | head -1); echo; }

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  tasks: [{builtin.debug: {msg: x}}]
YML
run "X1 - builtin.debug: {...}         (module-as-key)"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  tasks: [{action: {module: builtin.debug, msg: x}}]
YML
run "X2 - action: {module: builtin.debug}"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  vars: {m: builtin.debug}
  tasks: [{action: "{{ m }}"}]
YML
run "X3 - action: '{{ m }}' templated to builtin.debug"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  tasks: [{nosuchmodule: }]
YML
run "X4 - nosuchmodule: (1-part, unknown)"
