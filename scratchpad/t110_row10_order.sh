#!/usr/bin/env bash
# Row 10: is "duplicate loop in task" order-dependent? _preprocess_with_loop raises only
# when loop/loop_with is ALREADY set, and preprocess_data walks ds in written order.
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
    echo "RAN CLEAN -- iterations:"
    echo "$out" | grep -E '^ok: \[localhost\] =>' | sed 's/ =>.*//;s/^/    /'
    echo "$out" | grep -oE '\(item=[^)]*\)' | sed 's/^/    /'
  fi
  echo
}

run "loop: then with_items:" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: "{{ item }}"}
      loop: [1, 2]
      with_items: [a, b]'

run "with_items: then loop:" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: "{{ item }}"}
      with_items: [a, b]
      loop: [1, 2]'

run "with_items: then with_list:" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: "{{ item }}"}
      with_items: [a, b]
      with_list: [c, d]'

run "with_list: then with_items:" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: "{{ item }}"}
      with_list: [c, d]
      with_items: [a, b]'

# Does the surviving loop keep the OTHER key's lookup? with_items flattens, plain loop
# does not -- so a nested list tells us which one is in force.
run "with_items: then loop: -- nested, to see if loop_with survives" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: "{{ item }}"}
      with_items: [x]
      loop: [[1, 2], [3]]'

run "control: plain loop: with a nested list" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: "{{ item }}"}
      loop: [[1, 2], [3]]'
rm -rf "$D"
