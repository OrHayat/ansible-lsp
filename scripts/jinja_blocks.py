"""Generate the golden block splits `jinja::template` is checked against.

Same idea as `jinja_tokens.py` and `jinja_ast.py`: run jinja2 3.1.6's own lexer over a corpus
and write one JSON object per line, which `template.rs` reads with `include_str!`.

The comparison is on the *block split* — the sequence of (kind, content) a template reduces to
— rather than on byte offsets, because upstream folds trimmed whitespace into its delimiter
tokens (`block_end` comes back as `'-%}  '`) and rebuilding offsets from token lengths would
be reconstructing the answer rather than checking it.

Entry point is `Environment.lex`, which is `Lexer.tokeniter` — the raw scanner, before `wrap`.
That is the layer where `{% raw %}` is a state and a `%}` inside a string is not a terminator,
which is exactly what is being ported.

    python scripts/jinja_blocks.py <jinja2-sdist>/tests demo \\
        > crates/ansible-core/src/jinja/template_corpus.jsonl
"""

import json
import pathlib
import sys

from jinja2 import Environment, meta

# `keep_trailing_newline` is False in stock jinja2 and True in the thing we model. Measured on
# ansible-core 2.21.2: a template file ending `hello {{ name }}\n` renders to `hello world\n`,
# newline intact. Configuring the oracle to match Ansible is the point -- comparing against a
# default Environment would make this port "wrong" about a byte Ansible keeps.
env = Environment(keep_trailing_newline=True)

# Hand-written cases, each aimed at one lexical rule that a `{%`-search would get wrong.
ADVERSARIAL = [
    # the four traps named in T-040
    "{% raw %}{% include 'x.j2' %}{% endraw %}",
    "{# {% include 'x.j2' %} #}",
    "{% if x == '%}' %}ok{% endif %}",
    '{% if x == "%}" %}ok{% endif %}',
    "{% set xs = [1, 2] %}",
    # whitespace control, every marker
    "a  {%- if x -%}  b  {%- endif -%}  c",
    "a  {%+ if x +%}  b  {% endif %}  c",
    "a {{- x -}} b",
    "a {#- c -#} b",
    # raw, awkwardly
    "{% raw %}{% endraw %}",
    "{% raw %}a{% endraw %}b{% raw %}c{% endraw %}",
    "{%- raw -%}  x  {%- endraw -%}",
    "{% raw %}{% raw %}{% endraw %}",
    # comments
    "{# a #}{# b #}",
    "{##}",
    "{# unclosed",
    # unterminated everything
    "{% if x %}",
    "{{ x",
    "{% for x in xs %}",
    # a tag whose body contains the other delimiters
    "{% set a = '{{' %}",
    "{% set a = '{%' %}",
    "{{ '}}' }}",
    # empty and degenerate
    "",
    "no tags at all",
    "{{}}",
    "{%%}",
    # nesting and brackets
    "{{ xs[1] }}",
    "{{ {'a': 1} }}",
    "{{ f(a, [1, 2], {'k': 'v'}) }}",
    # the four target-naming tags this ticket exists for
    "{% extends 'base.j2' %}",
    "{% include 'partials/header.j2' %}",
    "{% import 'macros.j2' as m %}",
    "{% from 'macros.j2' import a, b as c %}",
    "{% include some_var %}",
    "{% include ['a.j2', 'b.j2'] %}",
    "{% include 'a.j2' ignore missing %}",
    "{% include 'a.j2' with context %}",
    # multi-line
    "line one\n{% if x %}\n  body\n{% endif %}\nline two",
    "{{\n  x\n}}",
]


def blocks(src):
    """Reduce a token stream to [kind, content] pairs."""
    out = []
    pending = None
    for _, tok, value in env.lex(src):
        if tok == "data":
            out.append(["data", value])
        elif tok in ("comment_begin", "block_begin", "variable_begin"):
            pending = (tok.rsplit("_", 1)[0], [])
        elif tok in ("comment_end", "block_end", "variable_end"):
            kind, parts = pending
            kind = {"comment": "comment", "block": "statement", "variable": "expression"}[kind]
            out.append([kind, "".join(parts)])
            pending = None
        elif tok in ("raw_begin", "raw_end"):
            # `raw` is a lexer state upstream and here: the tag itself is not a statement and
            # its body arrives as `data`, which the branch above already recorded.
            pending = None
        elif pending is not None:
            pending[1].append(value)
    return out


BODY_SUFFIXES = (".j2", ".jinja", ".jinja2", ".tmpl", ".html", ".txt")


def harvest(paths):
    seen = []
    for p in paths:
        if not p.is_file() or p.suffix.lower() not in BODY_SUFFIXES:
            continue
        try:
            text = p.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        if text.strip():
            seen.append(text)
    return seen


def parse_error(src):
    """What `env.parse` says, which is what decides whether the template renders."""
    try:
        env.parse(src)
        return None
    except Exception as e:  # noqa: BLE001 - any refusal is a refusal
        return str(e).splitlines()[0]


def referenced(src):
    """`jinja2.meta.find_referenced_templates`, the ready-made oracle for T-040.

    Yields `None` for a name it cannot resolve statically -- a dynamic `{% include var %}` --
    which is the same answer this port gives by refusing to call it a reference.
    """
    try:
        return list(meta.find_referenced_templates(env.parse(src)))
    except Exception:  # noqa: BLE001 - a template that does not parse names nothing
        return None


def row(src):
    src = src.replace("\r\n", "\n").replace("\r", "\n")
    # Two oracles, because they answer different questions. `lex` gives the block split; it is
    # lazy and simply stops on an unterminated tag, emitting nothing and raising nothing --
    # `'{{ x'` comes back as no tokens at all. `parse` is the one that says whether the
    # template renders, and on that same input it says exactly what this port says.
    out = {"src": src, "parse_err": parse_error(src), "refs": referenced(src)}
    try:
        out["blocks"] = blocks(src)
    except Exception as e:  # noqa: BLE001 - any refusal is a refusal
        out["lex_err"] = str(e).splitlines()[0]
    return out


def main():
    corpus = list(ADVERSARIAL)
    for root in sys.argv[1:]:
        corpus += harvest(pathlib.Path(root).rglob("*"))

    seen, written = set(), 0
    skipped = {}
    for src in corpus:
        if src in seen:
            continue
        seen.add(src)
        r = row(src)
        try:
            line = json.dumps(r)
        except ValueError as e:  # lone surrogates and friends
            skipped[str(e)] = skipped.get(str(e), 0) + 1
            continue
        print(line)
        written += 1
    # Never drop silently: a corpus that quietly discards what it cannot map reads as full
    # coverage and is not.
    sys.stderr.write("%d rows, %d skipped %s\n" % (written, sum(skipped.values()), skipped))


main()
