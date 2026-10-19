"""Generate the golden name sets `jinja::names` is checked against (T-212).

For every source, `jinja2.meta.find_undeclared_variables` on the parsed template -- the names
jinja2 resolves from the render context -- or the parse error. Meta is not the truth in every
shape: `names.rs` documents where it disagrees with rendering, and its test allows exactly
those divergences.

Sources: every `src` of `template_corpus.jsonl` (jinja2's own test templates plus the demo
tree), and the scoping cases below, each aimed at one binding rule.

    python scripts/jinja_names.py > crates/ansible-core/src/jinja/names_corpus.jsonl

Given tree paths instead, the sources are those trees' `.j2` files and every YAML scalar
holding a `{{` or `{%` -- the input to the env-gated `names_corpus_gate` test, never
committed:

    python scripts/jinja_names.py <tree>... > /tmp/names_corpus.jsonl
"""

import json
import pathlib
import sys

import yaml

from jinja2 import Environment, StrictUndefined, meta

env = Environment(undefined=StrictUndefined, keep_trailing_newline=True)


class AnyName(dict):
    """Every filter and test name exists. `find_undeclared_variables` compiles the template,
    and the compiler refuses a filter it does not know -- which is every Ansible filter. What
    a filter *is* never changes which names are looked up."""

    def __contains__(self, key):
        return True


env.filters = AnyName(env.filters)
env.tests = AnyName(env.tests)

SCOPING = [
    "{% for h in groups.web %}{{ h }}:{{ port }} {% endfor %}",
    "{% for h in xs %}{% endfor %}{{ h }}",
    "{% for h in xs %}{{ loop.index }}{% endfor %}",
    "{{ loop.index }}",
    "{% for h in xs if h.ok %}{{ h }}{% endfor %}",
    "{% for h in xs if loop %}{% endfor %}",
    "{% for h in xs %}{% else %}{{ h }}{{ loop }}{% endfor %}",
    "{% for k, v in d.items() %}{{ k }}{{ v }}{% endfor %}",
    "{% for (a, (b, c)) in xs %}{{ a }}{{ c }}{% endfor %}",
    "{% for i in i %}{% endfor %}",
    "{% for i in t recursive %}{{ loop(i.c) }}{% endfor %}{{ loop }}",
    "{% for x in xs %}{% for x in x %}{{ x }}{% endfor %}{{ x }}{% endfor %}",
    "{% for i in xs %}{% set q = i %}{{ q }}{% endfor %}{{ q }}",
    "{% for i in xs %}{{ q }}{% set q = i %}{% endfor %}",
    "{% set q = 1 %}{{ q }}",
    "{{ q }}{% set q = 1 %}",
    "{% set q = q + 1 %}",
    "{% set a, b = 1, 2 %}{{ a }}{{ b }}",
    "{% set q %}{{ r }}{% endset %}{{ q }}",
    "{% set q | upper %}x{% endset %}{{ q }}",
    "{% set ns = namespace(a=1) %}{% set ns.a = 2 %}{{ ns.a }}",
    "{% set ns.x = 1 %}",
    "{% if c %}{% set q = 1 %}{{ q }}{% endif %}",
    "{% if c %}{% set q = 1 %}{% else %}{{ q }}{% endif %}",
    "{% if c %}{% set q = 1 %}{% else %}{% set q = 2 %}{% endif %}{{ q }}",
    "{% if a %}{% elif b %}{% else %}{% endif %}",
    "{% with a = 1, b = a %}{{ a }}{{ b }}{% endwith %}{{ a }}",
    "{% with a = z %}{{ a }}{% endwith %}",
    "{% with %}{% set q = 1 %}{% endwith %}{{ q }}",
    "{% macro f(p, q=d) %}{{ p }}{{ q }}{{ r }}{{ varargs }}{{ kwargs }}{{ caller() }}{% endmacro %}{{ f(1) }}{{ p }}",
    "{% macro f(a, b=a) %}{% endmacro %}",
    "{% macro f(n) %}{{ f(n - 1) }}{% endmacro %}",
    "{% macro f() %}{{ q }}{% endmacro %}{% set q = 1 %}",
    "{% for i in xs %}{% macro f() %}{{ i }}{{ loop }}{% endmacro %}{% endfor %}",
    "{% call(u) f(xs) %}{{ u }}{% endcall %}",
    "{% macro f() %}{% endmacro %}{% for i in xs %}{% call f() %}{{ i }}{% endcall %}{% endfor %}",
    "{% filter replace(a, b) %}{{ c }}{% endfilter %}",
    "{% for i in xs %}{% filter upper %}{{ i }}{% endfilter %}{% endfor %}",
    "{% block body %}{{ x }}{{ super() }}{% endblock %}",
    "{% block b %}{% set q = 1 %}{% endblock %}{{ q }}",
    "{% import 'm' as m %}{{ m.f() }}",
    "{% from 'm' import a, b as c %}{{ a }}{{ b }}{{ c }}",
    "{% include tpl %}",
    "{% include ['a', b] ignore missing with context %}",
    "{% extends base %}",
    "{% autoescape ae %}{{ x }}{% endautoescape %}",
    "{{ xs | map(attribute='a') | list }}",
    "{{ x | default(y) }}{{ x is divisibleby n }}",
    "{{ lookup('x') }}{{ o.m(a) }}",
    "{{ {'a': b} }}{{ xs[a:b] }}{{ (c, d) }}{{ e if f else g }}",
    "{{ f(*a, **b) }}",
    "{% raw %}{{ nope }}{% endraw %}",
    "{# {{ nope }} #}",
]


def row(src):
    try:
        names = sorted(meta.find_undeclared_variables(env.parse(src)))
    except Exception as e:
        return {"src": src, "meta": None, "err": str(e)}
    return {"src": src, "meta": names}


def scalars(node):
    if isinstance(node, yaml.ScalarNode):
        yield node.value
    elif isinstance(node, yaml.SequenceNode):
        for n in node.value:
            yield from scalars(n)
    elif isinstance(node, yaml.MappingNode):
        for k, v in node.value:
            yield from scalars(k)
            yield from scalars(v)


def tree_sources(root):
    for path in sorted(pathlib.Path(root).rglob("*")):
        try:
            if not path.is_file() or ".git" in path.parts:
                continue
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        if path.suffix == ".j2":
            yield text
        elif path.suffix in (".yml", ".yaml"):
            try:
                docs = list(yaml.compose_all(text, Loader=yaml.SafeLoader))
            except yaml.YAMLError:
                continue
            for doc in docs:
                if doc is not None:
                    yield from (v for v in scalars(doc) if "{{" in v or "{%" in v)


def main():
    if len(sys.argv) > 1:
        seen = set()
        for root in sys.argv[1:]:
            for src in tree_sources(root):
                if src not in seen:
                    seen.add(src)
                    sys.stdout.write(json.dumps(row(src), ensure_ascii=False) + "\n")
        return
    here = pathlib.Path(__file__).resolve().parent.parent
    corpus = here / "crates/ansible-core/src/jinja/template_corpus.jsonl"
    seen = set()
    sources = list(SCOPING)
    for line in corpus.read_text(encoding="utf-8").splitlines():
        if line.strip():
            sources.append(json.loads(line)["src"])
    out = sys.stdout
    for src in sources:
        if src in seen:
            continue
        seen.add(src)
        out.write(json.dumps(row(src), ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
