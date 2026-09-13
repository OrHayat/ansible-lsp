#!/usr/bin/env bash
# T-067: the role search list, measured. Run from /tmp so ansible.cfg is honoured.
set -u
P=/tmp/t067so; rm -rf $P
role() { mkdir -p $1/tasks; echo "- debug: {msg: \"RAN $2\"}" > $1/tasks/main.yml; }
echo "localhost ansible_connection=local" > /tmp/t067hosts
run() { printf '%-60s ' "$1"; (cd ${3:-$P} && ansible-playbook -i /tmp/t067hosts $2 2>&1 | grep -oE '"msg": "RAN [^"]*"|was not found|ERROR[^\n]{0,80}' | tr '\n' ' '); echo; }

# ---- A. order: <playbook_dir>/roles vs roles_path
mkdir -p $P/a/playbooks
role $P/a/playbooks/roles/dup "playbook_dir/roles"
role $P/a/cfgroles/dup "roles_path"
printf '[defaults]\nroles_path = ./cfgroles\n' > $P/a/ansible.cfg
printf -- '- hosts: all\n  gather_facts: false\n  roles: [dup]\n' > $P/a/playbooks/site.yml
run "A1 dup in playbooks/roles AND roles_path" playbooks/site.yml $P/a
rm -rf $P/a/playbooks/roles
run "A2 control: only roles_path" playbooks/site.yml $P/a

# ---- B. roles_path vs <playbook_dir> itself
mkdir -p $P/b/playbooks
role $P/b/playbooks/dup "playbook_dir itself"
role $P/b/cfgroles/dup "roles_path"
printf '[defaults]\nroles_path = ./cfgroles\n' > $P/b/ansible.cfg
printf -- '- hosts: all\n  gather_facts: false\n  roles: [dup]\n' > $P/b/playbooks/site.yml
run "B1 dup in roles_path AND playbook_dir" playbooks/site.yml $P/b

# ---- C. sibling roles: A reaches B, where B is NOT on any listed root
# layout: $P/c/lib/{a,b}; roles_path points only at lib/a's parent? no — point at a dir holding only 'a'
mkdir -p $P/c/playbooks $P/c/other
role $P/c/other/a "a"; role $P/c/other/b "b"
printf '[defaults]\nroles_path = ./nowhere\n' > $P/c/ansible.cfg; mkdir -p $P/c/nowhere
# play names role a by absolute path so a loads; b only reachable as a's sibling
printf -- "dependencies: [b]\n" > $P/c/other/a/meta/main.yml 2>/dev/null || { mkdir -p $P/c/other/a/meta; printf -- "dependencies: [b]\n" > $P/c/other/a/meta/main.yml; }
printf -- "- hosts: all\n  gather_facts: false\n  roles: [$P/c/other/a]\n" > $P/c/playbooks/site.yml
run "C1 meta dependency on sibling b" playbooks/site.yml $P/c

rm -f $P/c/other/a/meta/main.yml
printf -- "- debug: {msg: \"RAN a\"}\n- include_role: {name: b}\n" > $P/c/other/a/tasks/main.yml
run "C2 include_role: b from inside a's tasks" playbooks/site.yml $P/c

printf -- "- debug: {msg: \"RAN a\"}\n- import_role: {name: b}\n" > $P/c/other/a/tasks/main.yml
run "C3 import_role: b from inside a's tasks" playbooks/site.yml $P/c

printf -- "- debug: {msg: \"RAN a\"}\n" > $P/c/other/a/tasks/main.yml
printf -- "- hosts: all\n  gather_facts: false\n  roles: [$P/c/other/a, b]\n" > $P/c/playbooks/site.yml
run "C4 control: play-level roles: [.., b] (b not on list)" playbooks/site.yml $P/c

# ---- D. demo, copied so its ansible.cfg is read
rm -rf /tmp/t067demo; cp -r /mnt/c/Users/orhay/ansible-lsp/demo /tmp/t067demo
run "D1 demo roles: - demo, cfg honoured" "--syntax-check playbook.yml" /tmp/t067demo
(cd /tmp/t067demo && ansible-playbook --syntax-check playbook.yml 2>&1 | grep -oE "not found in: .*|world writable" | head -2)
