#!/usr/bin/env bash
# Which role-name shapes does the unfrackpath fallback uniquely find?
# Three builds: original 2.21.2, PR #87268, and the fallback deleted outright.
set -u
SITE=/home/orhayat/.local/share/uv/tools/ansible-core/lib/python3.12/site-packages
PY=/home/orhayat/.local/share/uv/tools/ansible-core/bin/python
build() { rm -rf $2; mkdir -p $2; cp -r $SITE/ansible $2/; F=$2/ansible/playbook/role/definition.py; eval "$3"; }
build x /tmp/b_pr   'sed -i "s|role_path = unfrackpath(role_name)\$|role_path = unfrackpath(role_name, basedir=self._loader.get_basedir())|" $F'
build x /tmp/b_none 'sed -i "s|role_path = unfrackpath(role_name)\$|role_path = \"/nonexistent/fallback-removed\"|" $F'
grep -c "basedir=self._loader.get_basedir())" /tmp/b_pr/ansible/playbook/role/definition.py
grep -c "fallback-removed" /tmp/b_none/ansible/playbook/role/definition.py

P=/tmp/t067fs; rm -rf $P; mkdir -p $P/playbooks $P/shared/myrole/tasks ~/t067roles/homerole/tasks
echo '- debug: {msg: "RAN"}' > $P/shared/myrole/tasks/main.yml
echo '- debug: {msg: "RAN"}' > ~/t067roles/homerole/tasks/main.yml
echo "localhost ansible_connection=local" > $P/hosts

try() { # try <label> <role-name-as-yaml>
  printf -- "- hosts: all\n  gather_facts: false\n  roles: ['%s']\n" "$2" > $P/playbooks/site.yml
  printf '%-28s' "$1"
  for b in $SITE /tmp/b_pr /tmp/b_none; do
    r=$(cd $P && PYTHONPATH=$b $PY -c 'import sys; from ansible.cli.playbook import main; sys.argv=["ansible-playbook"]+sys.argv[1:]; main()' -i $P/hosts $P/playbooks/site.yml 2>&1 | grep -oE '"msg": "RAN"|was not found' | head -1)
    [ "$r" = '"msg": "RAN"' ] && printf '%-12s' found || printf '%-12s' "-"
  done; echo
}
printf '%-28s%-12s%-12s%-12s\n' "name (cwd = project)" original PR removed
try "relative  shared/myrole"  "shared/myrole"
try "absolute"                 "$P/shared/myrole"
try "tilde  ~/t067roles/..."   "~/t067roles/homerole"
try "env  \$HOME/t067roles/..." '$HOME/t067roles/homerole'
