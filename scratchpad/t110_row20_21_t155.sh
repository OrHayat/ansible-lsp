#!/usr/bin/env bash
# Rows 20 and 21, plus T-155's edges. ansible-core 2.21.2.
PB=/home/orhayat/.local/share/uv/tools/ansible-core/bin/ansible-playbook
D=$(mktemp -d)
cd "$D" || exit 1
printf -- '- debug: {msg: inc}\n' > inc.yml
mkdir -p roles/r/tasks
printf -- '- debug: {msg: role}\n' > roles/r/tasks/main.yml

run() {
  printf '%-46s ' "$1"; shift
  printf '%s\n' "$*" > c.yml
  out=$($PB -i localhost, c.yml 2>&1)
  if echo "$out" | grep -q '^\[ERROR\]'; then
    echo "$out" | grep '^\[ERROR\]' | head -1 | cut -c9-100
  else
    n=$(echo "$out" | grep -c '^ok: \[localhost\]')
    w=$(echo "$out" | grep -c '^\[WARNING\]: The variable')
    echo "clean (${n} ok, ${w} loop-var warnings)"
  fi
}

echo "--- row 20: a with_* with no value ---"
run "with_items: (null), alone" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: x}
      with_items:'
run "with_items: \"\" (empty string)" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: x}
      with_items: ""'
run "with_items: [] (empty list)" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: x}
      with_items: []'
run "with_dict: (null)" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: x}
      with_dict:'

echo
echo "--- row 21: loop_control that is not a dict ---"
run "loop_control: a-string, with a loop" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: x}
      loop: [1]
      loop_control: nonsense'
run "loop_control: a-string, no loop" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: x}
      loop_control: nonsense'
run "loop_control: [a, b]" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: x}
      loop: [1]
      loop_control: [a, b]'
run "loop_control: (null)" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: x}
      loop: [1]
      loop_control:'
run "loop_control: \"{{ a_var }}\"" '- hosts: localhost
  gather_facts: false
  vars: {a_var: {loop_var: it}}
  tasks:
    - debug: {msg: x}
      loop: [1]
      loop_control: "{{ a_var }}"'

echo
echo "--- T-155: loop_control with nothing to control ---"
run "plain task, loop_control, no loop" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: x}
      loop_control: {loop_var: it}'
run "include_tasks, loop_control, no loop" '- hosts: localhost
  gather_facts: false
  tasks:
    - include_tasks: inc.yml
      loop_control: {loop_var: it}'
run "include_role, loop_control, no loop" '- hosts: localhost
  gather_facts: false
  tasks:
    - include_role: {name: r}
      loop_control: {loop_var: it}'
run "import_tasks, loop_control, no loop" '- hosts: localhost
  gather_facts: false
  tasks:
    - import_tasks: inc.yml
      loop_control: {loop_var: it}'
run "block, loop_control, no loop" '- hosts: localhost
  gather_facts: false
  tasks:
    - block:
        - debug: {msg: x}
      loop_control: {loop_var: it}'
run "GOOD: loop_control WITH a loop" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: "{{ it }}"}
      loop: [1]
      loop_control: {loop_var: it}'
rm -rf "$D"
