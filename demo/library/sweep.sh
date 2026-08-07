#!/bin/bash
# A module is ANY executable, not just Python (T-093). A bare `sweep:` task resolves
# here: the legacy loader matches library/ files by base name with any extension —
# .sh, .ps1, or none at all (loader.py:899-927). Before T-093 only sweep.py would
# have been found and this file was invisible.
#
# Old-style non-Python modules get their args as a key=value file in $1.
source "$1" 2>/dev/null
echo "{\"changed\": false, \"msg\": \"swept ${paths:-nothing}\"}"
