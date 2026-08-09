#!/usr/bin/env bash
# Is `with_X -> loop:` a mechanical rewrite? Probe the two that look easiest.
PB=/home/orhayat/.local/share/uv/tools/ansible-core/bin/ansible-playbook
D=$(mktemp -d)
cd "$D" || exit 1

run() {
  echo "=== $1 ==="; shift
  printf '%s\n' "$*" > c.yml
  $PB -i localhost, c.yml 2>&1 | grep -E '^(ok|fatal|\[ERROR\])' | head -5
  echo
}

run "with_random_choice: the original" '- hosts: localhost
  gather_facts: false
  vars: {my_list: [a, b, c]}
  tasks:
    - debug: {msg: "{{ item }}"}
      with_random_choice: "{{ my_list }}"'

run "the naive rewrite: loop: {{ my_list | random }}" '- hosts: localhost
  gather_facts: false
  vars: {my_list: [a, b, c]}
  tasks:
    - debug: {msg: "{{ item }}"}
      loop: "{{ my_list | random }}"'

run "with_items: does it flatten?" '- hosts: localhost
  gather_facts: false
  vars: {nested: [[a, b], [c]]}
  tasks:
    - debug: {msg: "{{ item }}"}
      with_items: "{{ nested }}"'

run "the naive rewrite: loop: {{ nested }}" '- hosts: localhost
  gather_facts: false
  vars: {nested: [[a, b], [c]]}
  tasks:
    - debug: {msg: "{{ item }}"}
      loop: "{{ nested }}"'
rm -rf "$D"
