#!/usr/bin/env bash
# Row 10, remaining questions: does the duplicate check beat the null-value check, and does
# the discarded with_* really contaminate the surviving loop's semantics?
PB=/home/orhayat/.local/share/uv/tools/ansible-core/bin/ansible-playbook
D=$(mktemp -d)
cd "$D" || exit 1

run() {
  echo "=== $1 ==="; shift
  printf '%s\n' "$*" > c.yml
  out=$($PB -i localhost, c.yml 2>&1)
  if echo "$out" | grep -q '^\[ERROR\]'; then
    echo "$out" | grep '^\[ERROR\]' | head -1 | cut -c1-90
  else
    echo "RAN CLEAN:"; echo "$out" | grep -oE '\(item=[^)]*\)' | sed 's/^/    /'
  fi
  echo
}

run "loop: then a NULL with_items: -- which check wins?" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: "{{ item }}"}
      loop: [1, 2]
      with_items:'

run "a NULL with_items: alone -- row 20" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: "{{ item }}"}
      with_items:'

run "a NULL with_items: then loop: -- null on the discarded one" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: "{{ item }}"}
      with_items:
      loop: [1, 2]'

run "with_together: then loop: -- does zip contaminate?" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: "{{ item }}"}
      with_together: [[1], [2]]
      loop: [[a, b], [c, d]]'

run "control: loop: alone with that value" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: "{{ item }}"}
      loop: [[a, b], [c, d]]'
rm -rf "$D"
