#!/bin/bash
# Collections can ship non-Python modules too (T-093): the FQCN finder tries the exact
# name, then a sorted glob of `pulse.*` (loader.py:704-719) — same any-extension rule
# as legacy library/ dirs. So `demo.charlie.pulse:` resolves to this bash file.
# Only action plugins are controller-side Python classes and stay `.py`-bound.
source "$1" 2>/dev/null
echo "{\"changed\": false, \"msg\": \"pulse every ${interval:-60}s\"}"
