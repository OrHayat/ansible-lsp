#!/usr/bin/env bash
# T-067 item 4: is <playbook_dir> itself (no /roles) a role root?
set -u
P=/tmp/t067pd; rm -rf $P; mkdir -p $P/playbooks $P/nowhere
echo "localhost ansible_connection=local" > $P/hosts
printf '[defaults]\nroles_path = ./nowhere\n' > $P/ansible.cfg
printf -- '- hosts: all\n  gather_facts: false\n  roles: [dup]\n' > $P/playbooks/site.yml
mkdir -p $P/playbooks/dup/tasks
echo '- debug: {msg: "RAN playbook_dir itself"}' > $P/playbooks/dup/tasks/main.yml
run() { printf '%-44s ' "$1"; (cd $P && ansible-playbook -i hosts playbooks/site.yml 2>&1 | grep -oE '"msg": "RAN [^"]*"|was not found in: .*' | head -1); echo; }
run "B2 dup only in <playbook_dir>/dup"
rm -rf $P/playbooks/dup
run "B3 control: removed"
