# T-233: which builtin modules and documented options each core ships, and whether the docs'
# version_added / removal story matches. Reads gen/core-*.json and gen/doc-*.json written by
# t233_probe.sh.
import json
V = [f"2.{m}" for m in range(14, 22)]
cores = {v: json.load(open(f"gen/core-{v}.json")) for v in V}
print("cores:", [cores[v]["core"] for v in V])
for m in sorted(set().union(*(c["modules"] for c in cores.values()))):
    have = [v for v in V if m in cores[v]["modules"]]
    if have != V:
        print(f"  module {m}: {have[0]}..{have[-1]} ({len(have)}/{len(V)})")
docs = {v: json.load(open(f"gen/doc-{v}.json")) for v in V}
print("modules documented per core:", {v: len(docs[v]) for v in V})
key = lambda s: [int(x) for x in str(s).split(".")[:2]] if str(s)[:1].isdigit() else [0, 0]
def opts(d, prefix=""):
    out = {}
    for k, o in (d or {}).items():
        out[prefix + k] = o
        out.update(opts(o.get("suboptions"), prefix + k + "."))
    return out
present, meta = {}, {}
for v in V:
    for full, entry in docs[v].items():
        mod = full.split(".")[-1]
        for path, o in opts((entry.get("doc") or {}).get("options")).items():
            present.setdefault((mod, path), []).append(v)
            meta[(mod, path)] = (v, o)
print(f"option paths seen: {len(present)}; in all 8 cores: {sum(vs == V for vs in present.values())}")
ok, bad, unmarked = [], [], []
for (mod, path), vs in present.items():
    if vs[0] == V[0] or mod in ("deb822_repository", "dnf5", "mount_facts"):
        continue  # present from the start, or the whole module is new
    va = meta[(mod, path)][1].get("version_added")
    (unmarked if va is None else ok if key(va) == key(vs[0]) else bad).append((mod, path, va, vs[0]))
print(f"\nADDED to an existing module after 2.14: {len(ok) + len(bad) + len(unmarked)}")
print(f"  version_added == first core that ships it: {len(ok)}")
print(f"  version_added disagrees: {len(bad)}")
for m, p, va, f in bad: print(f"     {m}.{p}: says {va}, first shipped {f}")
print(f"  no version_added: {len(unmarked)}")
for m, p, va, f in unmarked: print(f"     {m}.{p}: first shipped {f}")
removed = sorted((k, vs) for k, vs in present.items() if vs[-1] != V[-1] and k[0] not in ("yum", "_include"))
print(f"\nREMOVED from a module still shipped in 2.21: {len(removed)}")
for (mod, path), vs in removed:
    o = meta[(mod, path)][1]
    gaps = [v for v in V[V.index(vs[0]):V.index(vs[-1]) + 1] if v not in vs]
    print(f"  {mod}.{path}: {vs[0]}..{vs[-1]}{' gaps ' + ','.join(gaps) if gaps else ''}; deprecated marker in its last doc: {o.get('deprecated') or 'NONE'}")
