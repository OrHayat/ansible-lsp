#!/usr/bin/env bash
# T-110 rows 3 and 4: loops on import_tasks / import_role, measured on ansible-core 2.21.2.
PB=/home/orhayat/.local/share/uv/tools/ansible-core/bin/ansible-playbook
D=$(mktemp -d)
cd "$D" || exit 1
printf -- '- debug: {msg: imported}\n' > inc.yml
mkdir -p roles/r/tasks
printf -- '- debug: {msg: role}\n' > roles/r/tasks/main.yml

run() {
  name=$1; shift
  printf '%s\n' "$*" > case.yml
  echo "=== $name ==="
  out=$($PB --syntax-check -i localhost, case.yml 2>&1)
  echo "$out" | grep -E '^\[(ERROR|WARNING)\]' | grep -v 'Unable to parse|No inventory|provided hosts list' | head -2
  echo "$out" | grep -q '^\[ERROR\]' || echo "(clean)"
  echo
}

run "import_tasks + loop" '- hosts: localhost
  tasks:
    - import_tasks: inc.yml
      loop: [1, 2]'

run "import_tasks + with_items" '- hosts: localhost
  tasks:
    - import_tasks: inc.yml
      with_items: [1, 2]'

run "import_tasks + null loop" '- hosts: localhost
  tasks:
    - import_tasks: inc.yml
      loop:'

run "import_tasks + empty-list loop" '- hosts: localhost
  tasks:
    - import_tasks: inc.yml
      loop: []'

run "FQCN import_tasks + loop" '- hosts: localhost
  tasks:
    - ansible.builtin.import_tasks: inc.yml
      loop: [1]'

run "import_tasks in a block + loop" '- hosts: localhost
  tasks:
    - block:
        - import_tasks: inc.yml
          loop: [1]'

run "import_role + loop" '- hosts: localhost
  tasks:
    - import_role: {name: r}
      loop: [1, 2]'

run "import_role + with_items" '- hosts: localhost
  tasks:
    - import_role: {name: r}
      with_items: [1, 2]'

run "import_role + null loop" '- hosts: localhost
  tasks:
    - import_role: {name: r}
      loop:'

run "GOOD include_tasks + loop" '- hosts: localhost
  tasks:
    - include_tasks: inc.yml
      loop: [1, 2]'

run "GOOD include_role + loop" '- hosts: localhost
  tasks:
    - include_role: {name: r}
      loop: [1, 2]'

run "GOOD import_tasks, no loop" '- hosts: localhost
  tasks:
    - import_tasks: inc.yml'
rm -rf "$D"
