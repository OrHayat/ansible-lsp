#!/usr/bin/env bash
# T-110 batch 1: measure each rule's real message on ansible-core 2.21.2.
PB=/home/orhayat/.local/share/uv/tools/ansible-core/bin/ansible-playbook
D=$(mktemp -d)
cd "$D" || exit 1

run() {
  name=$1; shift
  printf '%s\n' "$*" > case.yml
  echo "=== $name ==="
  $PB --syntax-check -i localhost, case.yml 2>&1 | grep -Ev '^$|WARNING|^Origin:|^ *[0-9]+ |^ *\^|^$' | head -4
  echo
}

run "user+remote_user" '- hosts: web
  user: alice
  remote_user: bob
  tasks: []'

run "hosts null" '- hosts:
  tasks: []'

run "hosts empty string" '- hosts: ""
  tasks: []'

run "hosts empty list" '- hosts: []
  tasks: []'

run "hosts list with None" '- hosts:
    - web
    -
    - db
  tasks: []'

run "hosts list with mapping" '- hosts:
    - {name: web}
  tasks: []'

run "hosts mapping" '- hosts: {group: web}
  tasks: []'

run "hosts numeric" '- hosts: 42
  tasks: []'

run "vars_prompt missing name" '- hosts: localhost
  vars_prompt:
    - prompt: Password?
  tasks: []'

run "vars_prompt unsupported key" '- hosts: localhost
  vars_prompt:
    - name: pw
      promt: Password?
  tasks: []'

run "vars_prompt lone mapping no name" '- hosts: localhost
  vars_prompt:
    prompt: Password?
  tasks: []'

run "entry not a dict" '- hosts: localhost
  tasks: []
- just-a-string'

run "clean control" '- name: fine
  hosts: localhost
  remote_user: deploy
  vars_prompt:
    - name: pw
      prompt: Password?
  tasks:
    - debug: {msg: hi}'
rm -rf "$D"
