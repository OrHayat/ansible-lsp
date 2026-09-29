# T-242 — A plugin file that exists but cannot load reads as found

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P3       | M    | T-109      |

## Problem

`unknown-plugin` (T-109) copies the loader's first step only: `<type>_plugins/<name>.py`
exists. Ansible then imports the file and takes the type's class out of it
(`plugins/loader.py:1784-1904` — `PluginLoader('StrategyModule', …)`), and only that step
fails. So "no diagnostic" means "Ansible will pick this file", not "this is a plugin".

Measured on 2.21.3, each file in a `strategy_plugins/` beside the playbook, `strategy: <name>`:

| file                                   | we say | Ansible                                                                  |
| -------------------------------------- | ------ | ------------------------------------------------------------------------ |
| `rusty.py` — Rust source               | silent | `Unexpected Exception …: invalid syntax (rusty.py, line 1)`              |
| `linalg.py` — ordinary functions       | silent | `… module 'ansible.plugins.strategy.linalg' has no attribute 'StrategyModule'` |
| `noclass.py` — `x = 1`                 | silent | same `has no attribute 'StrategyModule'`                                 |
| `demo_steps.py` — subclasses `linear`  | silent | runs                                                                     |

Every flagged case passes `--syntax-check`; the failure is at run time, and it is fatal.

## Approach

Read the found file, and only the found file — it is one per keyword value, never a scan.

1. Parse it as Python. A syntax error fails the load. Needs a Python parser crate (ruff's
   `ruff_python_parser` is the candidate); there is none in the tree today.
2. Look for the type's class bound at module top level: `class StrategyModule`, `class
   Connection`, `class BecomeModule` (the loaders' first argument). The class name becomes a
   column in `config::PLUGIN_PATH_SETTINGS`.
3. A top-level import that binds the name (`from x import StrategyModule`, `import *`) cannot
   be followed into another package: treat it as found, silently. Same for a name bound by
   assignment or inside `try`/`if`.

Built-in plugins (the package folder) are not read — they are the install's own files.

Not in reach, and not this ticket: whether the file's imports resolve under the interpreter
Ansible runs (mitogen's `import ansible_mitogen`), and `required_base_class` — checking the
class derives from `StrategyBase` means resolving its bases through imports.

## Done when

- [ ] each measured row above is a test: the first three flagged with Ansible's own failure
      quoted, `demo_steps.py` silent
- [ ] a class reached only through an import is silent, asserted
- [ ] the class name per type lives on the plugin-type table, not in the rule
- [ ] the demo gains a broken local plugin and a row for it, pinned by the exact-set test
- [ ] measured again for `connection:` (a `connection_plugins/` file without `Connection`)
      before its row lands
