#!/usr/bin/env bash
# Row 1: where is "Using a block as a handler is not supported." actually reachable?
# load_list_of_blocks loads a top-level block itself; only load_list_of_tasks checks.
PB=/home/orhayat/.local/share/uv/tools/ansible-core/bin/ansible-playbook
D=$(mktemp -d)
cd "$D" || exit 1
printf -- '- name: h2\n  block:\n    - debug: {msg: in-included-block}\n' > blocky.yml

run() {
  printf '%-46s ' "$1"; shift
  printf '%s\n' "$*" > c.yml
  out=$($PB -i localhost, c.yml 2>&1)
  if echo "$out" | grep -q '^\[ERROR\]'; then
    echo "$out" | grep '^\[ERROR\]' | head -1 | cut -c9-86
  else
    echo "OK"
  fi
}

run "top-level block: in handlers:" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: t}
      changed_when: true
      notify: h
  handlers:
    - name: h
      block:
        - debug: {msg: x}'

run "block nested inside a handler block" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: t}
      changed_when: true
      notify: h
  handlers:
    - name: h
      block:
        - block:
            - debug: {msg: x}'

run "block in a rescue: of a handler block" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: t}
      changed_when: true
      notify: h
  handlers:
    - name: h
      block:
        - debug: {msg: x}
      rescue:
        - block:
            - debug: {msg: r}'

run "block inside a file include_tasks pulls in" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: t}
      changed_when: true
      notify: h
  handlers:
    - name: h
      include_tasks: blocky.yml'

run "control: nested block in TASKS is fine" '- hosts: localhost
  gather_facts: false
  tasks:
    - block:
        - block:
            - debug: {msg: x}'
rm -rf "$D"
