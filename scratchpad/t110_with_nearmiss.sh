#!/usr/bin/env bash
# Which with_* names does ansible recognize as loops? The suffix must name an installed
# lookup plugin (task.py:336) -- so typos and non-lookups fall through to invalid-attribute.
PB=/home/orhayat/.local/share/uv/tools/ansible-core/bin/ansible-playbook
D=$(mktemp -d)
cd "$D" || exit 1
printf -- '- debug: {msg: x}\n' > inc.yml
for k in with_item with_dicts with_cartesian with_env with_url with_template with_pipe with_varnames; do
  printf -- '- hosts: localhost\n  tasks:\n    - import_tasks: inc.yml\n      %s: [1]\n' "$k" > c.yml
  printf '%-16s ' "$k"
  $PB --syntax-check -i localhost, c.yml 2>&1 | grep '^\[ERROR\]' | head -1 | cut -c1-80
done
rm -rf "$D"
