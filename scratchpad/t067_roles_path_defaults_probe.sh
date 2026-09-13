#!/usr/bin/env bash
# T-067: does a set `roles_path` REPLACE ansible's default roles paths, or extend them?
set -u
R=/tmp/t067rp; rm -rf $R; mkdir -p $R/roles/local_role/tasks
H=~/.ansible/roles/home_role/tasks; mkdir -p $H
echo '- debug: {msg: "LOCAL roles/local_role"}' > $R/roles/local_role/tasks/main.yml
echo '- debug: {msg: "HOME ~/.ansible/roles/home_role"}' > $H/main.yml
echo "localhost ansible_connection=local" > $R/hosts
run() { printf '%-46s ' "$1"; (cd $R && ansible-playbook -i hosts p.yml 2>&1 | grep -oE '"msg": "[^"]*"|was not found' | head -1); echo; }

printf -- '- hosts: all\n  gather_facts: false\n  roles: [home_role]\n' > $R/p.yml

rm -f $R/ansible.cfg
run "home_role, NO ansible.cfg           (control)"

printf '[defaults]\nroles_path = ./roles\n' > $R/ansible.cfg
run "home_role, roles_path = ./roles"

printf '[defaults]\nroles_path = ./roles:.\n' > $R/ansible.cfg
run "home_role, roles_path = ./roles:."

printf -- '- hosts: all\n  gather_facts: false\n  roles: [local_role]\n' > $R/p.yml
run "local_role, roles_path = ./roles:.    (control)"

echo
echo "--- and what ansible PRINTS as its search list when a role is missing:"
printf -- '- hosts: all\n  gather_facts: false\n  roles: [nope]\n' > $R/p.yml
(cd $R && ansible-playbook -i hosts p.yml 2>&1 | grep -oE "was not found in: .*" | head -1)
