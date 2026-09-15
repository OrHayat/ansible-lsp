#!/bin/sh
# T-233: rerun the measurements. Needs uv and network (pulls each ansible-core minor into a
# throwaway env). Writes into ./t233-out/.
set -e
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p t233-out/gen && cd t233-out
for minor in 14 15 16 17 18 19 20 21; do
  # 2.14/2.15's ansible-doc cannot parse docs under Python 3.14.
  py=3.14; [ "$minor" -le 15 ] && py=3.11
  with="ansible-core>=2.$minor,<2.$((minor + 1))"
  uv run -q --python $py --no-project --with "$with" python "$here/t233_list_builtins.py" > gen/core-2.$minor.json
  names=$(python3 -c "import json;print(' '.join('ansible.builtin.'+m for m in json.load(open('gen/core-2.$minor.json'))['modules'] if m not in ('async_wrapper','_include')))")
  uv run -q --python $py --no-project --with "$with" ansible-doc -j -t module $names </dev/null > gen/doc-2.$minor.json
done
python3 "$here/t233_doc_option_drift.py"
# Enforced specs: the three shapes the ticket records.
uv run -q --python 3.14 --no-project --with "ansible-core>=2.19,<2.20" python "$here/t233_capture_argspec.py" module:dnf
uv run -q --python 3.14 --no-project --with "ansible-core>=2.17,<2.18" python "$here/t233_capture_argspec.py" module:git
uv run -q --python 3.14 --no-project --with "ansible-core>=2.20,<2.21" python "$here/t233_capture_argspec.py" action:script module:copy
