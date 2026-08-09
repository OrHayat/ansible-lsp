#!/usr/bin/env bash
# Does every documented with_<lookup> reach the loop-on-import rule, and what does a
# with_* that is NOT an installed lookup do? ansible-core 2.21.2.
PB=/home/orhayat/.local/share/uv/tools/ansible-core/bin/ansible-playbook
D=$(mktemp -d)
cd "$D" || exit 1
printf -- '- debug: {msg: imported}\n' > inc.yml

for k in with_list with_items with_indexed_items with_flattened with_together \
         with_dict with_sequence with_subelements with_nested with_cartesian \
         with_random_choice with_fileglob with_first_found with_lines with_frobnicate; do
  printf -- '- hosts: localhost\n  tasks:\n    - import_tasks: inc.yml\n      %s: [1]\n' "$k" > case.yml
  printf '%-22s ' "$k"
  out=$($PB --syntax-check -i localhost, case.yml 2>&1)
  if echo "$out" | grep -q 'cannot use loops'; then
    echo "LOOP  (loop-on-import error)"
  elif echo "$out" | grep -q '^\[ERROR\]'; then
    echo "OTHER $(echo "$out" | grep '^\[ERROR\]' | head -1 | cut -c1-72)"
  else
    echo "clean $(echo "$out" | grep '^\[WARNING\]: Ignoring' | head -1 | cut -c1-50)"
  fi
done
rm -rf "$D"
