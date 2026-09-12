#!/usr/bin/env bash
# T-042 item 4: does the play START for each 2-part spelling? A task before the bad one
# runs only if the failure is at run time, not parse time.
set -u
R=/tmp/t042tt; rm -rf $R; mkdir -p $R
echo "localhost ansible_connection=local" > $R/hosts
run() { echo "--- $1"; (cd $R && ansible-playbook -i hosts p.yml 2>&1 | grep -E "FIRST TASK RAN|couldn't resolve|Cannot resolve" | head -2); echo; }

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  tasks:
    - debug: {msg: FIRST TASK RAN}
    - builtin.debug: {msg: x}
YML
run "EA module-as-key  - builtin.debug:"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  tasks:
    - debug: {msg: FIRST TASK RAN}
    - action: {module: builtin.debug, msg: x}
YML
run "EB action-keyword - action: {module: builtin.debug}"

cat > $R/p.yml <<'YML'
- hosts: all
  gather_facts: false
  tasks:
    - debug: {msg: FIRST TASK RAN}
    - local_action: builtin.debug msg=x
YML
run "EC local_action   - local_action: builtin.debug"
