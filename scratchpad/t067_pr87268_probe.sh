#!/usr/bin/env bash
# Apply PR #87268's one-line change to a COPY of ansible-core and re-run the same fixture.
set -u
SITE=/home/orhayat/.local/share/uv/tools/ansible-core/lib/python3.12/site-packages
PY=/home/orhayat/.local/share/uv/tools/ansible-core/bin/python
PATCHED=/tmp/ansible_pr87268; rm -rf $PATCHED; mkdir -p $PATCHED
cp -r $SITE/ansible $PATCHED/
F=$PATCHED/ansible/playbook/role/definition.py
sed -i 's|role_path = unfrackpath(role_name)$|role_path = unfrackpath(role_name, basedir=self._loader.get_basedir())|' $F
grep -n "unfrackpath(role_name" $F

P=/tmp/t067p5; rm -rf $P; mkdir -p $P/playbooks $P/shared/myrole/tasks
echo '- debug: {msg: "RAN <project>/shared/myrole"}'           > $P/shared/myrole/tasks/main.yml
echo "localhost ansible_connection=local" > $P/hosts
printf -- '- hosts: all\n  gather_facts: false\n  roles: [shared/myrole]\n' > $P/playbooks/site.yml

run() { printf '  %-22s ' "cwd=$2"; (cd $2 && PYTHONPATH=$1 $PY -c 'import sys; from ansible.cli.playbook import main; sys.argv=["ansible-playbook"]+sys.argv[1:]; main()' -i $P/hosts $P/playbooks/site.yml 2>&1 | grep -oE '"msg": "[^"]*"|was not found' | head -1); echo; }

for v in original patched; do
  [ $v = original ] && PP=$SITE || PP=$PATCHED
  echo "--- $v"
  run $PP $P
  run $PP /tmp
  run $PP $P/playbooks
done
echo "--- absolute path, patched (control: basedir must not affect it)"
printf -- "- hosts: all\n  gather_facts: false\n  roles: [$P/shared/myrole]\n" > $P/playbooks/site.yml
run $PATCHED /tmp
