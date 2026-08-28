"""Harvest Jinja expressions from an ansible-core source tree.

jinja2's own tests are a corpus of *Jinja*. This is a corpus of the thing we actually parse:
`when:`, `changed_when:`, `failed_when:`, `until:` and `assert:`'s `that` — the five keywords
ansible-core routes to `_compile_expression` (`_engine.py:293`), which is
`Parser(state="variable")` plus an end-of-stream check and cannot contain a statement. Every
value of those keys is a bare expression by definition, so they need no unwrapping at all.

Also takes every `{{ }}` and `{% %}` body out of the same tree, which is the templated-scalar
and `.j2` surface (T-040's, but the expression inside a delimiter is ours).

Emits the same JSONL as `jinja_ast.py`, so the same Rust gate reads it.

    pip download ansible-core --no-binary :all: --no-deps -d /tmp/ac
    tar xzf /tmp/ac/*.tar.gz -C /tmp/ac
    python scripts/jinja_ansible_corpus.py /tmp/ac/ansible_core-*/test \\
        > $SCRATCH/ansible.jsonl
    JINJA_FUZZ_CORPUS=$SCRATCH/ansible.jsonl \\
        cargo test -p ansible-core --lib fuzz_corpus -- --ignored --nocapture

Not checked in: it is a few thousand rows harvested from a tree that is not vendored here,
and it moves with each ansible-core release. Run it, read the divergences, and promote
anything real into `jinja_ast.py`'s adversarial block where it becomes permanent.
"""

import json
import pathlib
import re
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from jinja_ast import row  # noqa: E402
from jinja_tokens import BODY  # noqa: E402

import yaml  # noqa: E402

# `config/base.yml:78-93` groups these as the embedded-template cases. `that` is a parameter
# of the `assert` module rather than a directive, which is why it is matched separately.
BARE = {"when", "changed_when", "failed_when", "until"}


def scalars(node, key=None):
    """Every bare-expression value in a parsed YAML document, however nested."""
    if isinstance(node, dict):
        for k, v in node.items():
            yield from scalars(v, k)
            if k == "assert" and isinstance(v, dict) and "that" in v:
                yield from as_clauses(v["that"])
    elif isinstance(node, list):
        for v in node:
            yield from scalars(v, key)
    elif key in BARE:
        yield from as_clauses(node)


def as_clauses(v):
    """A list value is several ANDed clauses; a scalar is one."""
    if isinstance(v, list):
        for item in v:
            if isinstance(item, str):
                yield item
    elif isinstance(v, str):
        yield v


def harvest_tree(root):
    bare, delimited, unreadable = [], [], 0
    for p in sorted(pathlib.Path(root).rglob("*")):
        if not p.is_file() or p.suffix not in {".yml", ".yaml", ".j2"}:
            continue
        try:
            text = p.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue

        for a, b in BODY.findall(text):
            body = (a or b).strip()
            if body:
                delimited.append(body)

        if p.suffix == ".j2":
            continue
        try:
            # Ansible's test tree contains deliberately broken YAML and templated keys that
            # are not YAML at all. Those are the parser's problem, not this script's.
            for doc in yaml.safe_load_all(text):
                bare.extend(scalars(doc))
        except Exception:  # noqa: BLE001
            unreadable += 1
    return bare, delimited, unreadable


def main():
    bare, delimited, unreadable = [], [], 0
    for root in sys.argv[1:]:
        b, d, u = harvest_tree(root)
        bare += b
        delimited += d
        unreadable += u

    # A `when:` may still be written with delimiters — Ansible deprecates it but the tree has
    # examples, and `classify` already reports it. Keep both spellings: the inner expression is
    # ours either way, and the wrapped form is a refusal on both sides.
    seen, rows, skipped = set(), [], {}
    for src in [s.strip() for s in bare + delimited]:
        if not src or src in seen:
            continue
        seen.add(src)
        r = row(src)
        if "skip" in r:
            skipped[r["skip"]] = skipped.get(r["skip"], 0) + 1
            continue
        rows.append(r)

    ok = sum(1 for r in rows if "ast" in r)
    print(f"skipped {sum(skipped.values())} {skipped}", file=sys.stderr)
    print(
        f"{len(rows)} rows ({ok} parse, {len(rows) - ok} refused) "
        f"from {len(set(bare))} bare-expression values and {len(set(delimited))} delimiter bodies; "
        f"{unreadable} files were not YAML",
        file=sys.stderr,
    )
    rows.sort(key=lambda r: r["src"])
    for r in rows:
        print(json.dumps(r, ensure_ascii=True, sort_keys=True))


if __name__ == "__main__":
    main()
