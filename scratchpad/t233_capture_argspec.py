# Capture the argument_spec a builtin module or action plugin actually validates against, by
# intercepting the validation call and aborting before any real work runs.
import importlib, json, runpy, sys, pathlib
from unittest import mock
from ansible.module_utils import basic
from ansible.plugins.action import ActionBase
from ansible.release import __version__

class Captured(Exception):
    def __init__(self, spec): self.spec = spec

def module_spec(name):
    def fake_init(self, argument_spec=None, **kw): raise Captured(argument_spec)
    with mock.patch.object(basic.AnsibleModule, "__init__", fake_init):
        path = pathlib.Path(importlib.util.find_spec("ansible").submodule_search_locations[0]) / "modules" / f"{name}.py"
        try:
            runpy.run_path(str(path), run_name="__main__")
        except Captured as c:
            return c.spec
        except SystemExit:
            return None
    return None

def action_spec(name):
    mod = importlib.import_module(f"ansible.plugins.action.{name}")
    def fake_validate(self, argument_spec=None, **kw): raise Captured(argument_spec)
    task = mock.MagicMock(); task.args = {}; task.async_val = 0; task.check_mode = False
    with mock.patch.object(ActionBase, "validate_argument_spec", fake_validate):
        try:
            mod.ActionModule(task, mock.MagicMock(), mock.MagicMock(), mock.MagicMock(), mock.MagicMock(), mock.MagicMock()).run(task_vars={})
        except Captured as c:
            return c.spec
        except Exception as e:
            return f"not captured: {type(e).__name__}"
    return "not captured: no validate_argument_spec call"

def slim(spec):
    keep = ("aliases", "removed_in_version", "removed_at_date", "deprecated_aliases", "type", "choices")
    return {k: {a: v[a] for a in keep if a in v} for k, v in (spec or {}).items()}

out = {"core": __version__}
for kind, name in (a.split(":") for a in sys.argv[1:]):
    spec = module_spec(name) if kind == "module" else action_spec(name)
    out[f"{kind}:{name}"] = slim(spec) if isinstance(spec, dict) else spec
print(json.dumps(out, indent=1))
