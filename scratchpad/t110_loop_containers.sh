#!/usr/bin/env bash
# Rows 3/4 again, but checking that every task container reaches the same loader.
PB=/home/orhayat/.local/share/uv/tools/ansible-core/bin/ansible-playbook
D=$(mktemp -d)
cd "$D" || exit 1
printf -- '- debug: {msg: imported}\n' > inc.yml

for key in pre_tasks tasks post_tasks handlers; do
  printf -- '- hosts: localhost\n  %s:\n    - import_tasks: inc.yml\n      loop: [1]\n' "$key" > case.yml
  echo "=== $key ==="
  $PB --syntax-check -i localhost, case.yml 2>&1 | grep '^\[ERROR\]' | head -1
  echo
done

echo "=== nested block ==="
printf -- '- hosts: localhost\n  tasks:\n    - block:\n        - import_tasks: inc.yml\n          loop: [1]\n' > case.yml
$PB --syntax-check -i localhost, case.yml 2>&1 | grep '^\[ERROR\]' | head -1

echo
echo "=== standalone task file, included ==="
printf -- '- import_tasks: inc.yml\n  loop: [1]\n' > tasklist.yml
printf -- '- hosts: localhost\n  tasks:\n    - import_tasks: tasklist.yml\n' > case.yml
$PB --syntax-check -i localhost, case.yml 2>&1 | grep '^\[ERROR\]' | head -1
rm -rf "$D"
