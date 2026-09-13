#!/usr/bin/env bash
# definition.py item 5: `unfrackpath(role_name)` — the role name treated as a path.
# Does a RELATIVE role path resolve against the process CWD, so the same playbook works
# from one directory and fails from another? (Run from /tmp: /mnt/c makes ansible.cfg
# be ignored as world-writable.)
set -u
P=/tmp/t067p5; rm -rf $P; mkdir -p $P/playbooks $P/shared/myrole/tasks
echo '- debug: {msg: "RAN shared/myrole"}' > $P/shared/myrole/tasks/main.yml
echo "localhost ansible_connection=local" > $P/hosts

run() { printf '%-56s ' "$1"; (cd "$2" && ansible-playbook -i $P/hosts $P/playbooks/site.yml 2>&1 \
  | grep -oE '"msg": "[^"]*"|was not found in: .*' | head -1); echo; }

# relative path, nothing else in the search list can reach it
printf -- '- hosts: all\n  gather_facts: false\n  roles: [shared/myrole]\n' > $P/playbooks/site.yml
run "relative 'shared/myrole', cwd = project root" $P
run "relative 'shared/myrole', cwd = /tmp" /tmp
run "relative 'shared/myrole', cwd = playbooks/" $P/playbooks

# absolute path: the control — must work from anywhere
printf -- "- hosts: all\n  gather_facts: false\n  roles: [$P/shared/myrole]\n" > $P/playbooks/site.yml
run "absolute path, cwd = /tmp" /tmp
run "absolute path, cwd = project root" $P
