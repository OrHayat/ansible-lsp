#!/usr/bin/env bash
# Batch 3: what IS allowed in a handler list, and what is not? ansible-core 2.21.2.
PB=/home/orhayat/.local/share/uv/tools/ansible-core/bin/ansible-playbook
D=$(mktemp -d)
cd "$D" || exit 1
printf -- '- debug: {msg: inc}\n' > inc.yml
mkdir -p roles/r/tasks roles/r/handlers
printf -- '- debug: {msg: role-task}\n' > roles/r/tasks/main.yml
printf -- '- name: rh\n  debug: {msg: role-handler}\n' > roles/r/handlers/main.yml

run() {
  printf '%-44s ' "$1"; shift
  printf '%s\n' "$*" > c.yml
  out=$($PB -i localhost, c.yml 2>&1)
  if echo "$out" | grep -q '^\[ERROR\]'; then
    echo "$out" | grep '^\[ERROR\]' | head -1 | cut -c9-88
  else
    echo "OK"
  fi
}

echo "--- inside handlers: ---"
for a in "include_tasks: inc.yml" "import_tasks: inc.yml" \
         "include_role: {name: r}" "import_role: {name: r}" \
         "ansible.builtin.include_role: {name: r}"; do
  run "$a" "- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: t}
      changed_when: true
      notify: h
  handlers:
    - name: h
      $a"
done

run "block:" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: t}
      changed_when: true
      notify: h
  handlers:
    - name: h
      block:
        - debug: {msg: in-block}'

run "meta: end_role" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: t}
      changed_when: true
      notify: h
  handlers:
    - name: h
      meta: end_role'

run "meta: flush_handlers" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: t}
      changed_when: true
      notify: h
  handlers:
    - name: h
      meta: flush_handlers'

echo
echo "--- the same actions inside tasks: ---"
for a in "include_role: {name: r}" "import_role: {name: r}"; do
  run "$a" "- hosts: localhost
  gather_facts: false
  tasks:
    - $a"
done
run "block: in tasks" '- hosts: localhost
  gather_facts: false
  tasks:
    - block:
        - debug: {msg: in-block}'

echo
echo "--- meta: end_role, in and out of a role ---"
run "meta: end_role in a play task list" '- hosts: localhost
  gather_facts: false
  tasks:
    - meta: end_role'
printf -- '- meta: end_role\n- debug: {msg: after}\n' > roles/r/tasks/main.yml
run "meta: end_role inside a role" '- hosts: localhost
  gather_facts: false
  roles:
    - r'
run "meta: end_role in a ROLE handlers file" '- hosts: localhost
  gather_facts: false
  tasks:
    - debug: {msg: t}
      changed_when: true
      notify: rh
  roles:
    - r'
rm -rf "$D"
