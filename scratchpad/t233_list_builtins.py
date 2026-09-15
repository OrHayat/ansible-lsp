# Run inside an environment that has ansible-core installed. Reads files only; imports nothing
# from ansible except the version string.
import importlib.util, json, pathlib, sys
pkg = pathlib.Path(importlib.util.find_spec("ansible").submodule_search_locations[0])
ns = {}
exec((pkg / "release.py").read_text(), ns)
names = lambda d: sorted(p.stem for p in (pkg / d).glob("*.py") if not p.name.startswith("__"))
json.dump({"core": ns["__version__"], "modules": names("modules"), "action": names("plugins/action")}, sys.stdout)
