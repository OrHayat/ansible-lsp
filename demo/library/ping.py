# A workspace-local module deliberately named like the builtin: pins that legacy
# `library/` dirs are searched BEFORE the ansible package ("package path always gets
# added last", loader.py:497), so a bare `ping:` task resolves here, not into core.
DOCUMENTATION = r"""
---
module: ping
short_description: demo shadow of the builtin ping
"""
