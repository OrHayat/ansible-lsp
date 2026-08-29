"""Generate the golden expression trees `jinja::parser` is checked against.

Same idea as `jinja_tokens.py` one level up: run jinja2 3.1.6's own parser over a corpus and
write one JSON object per line, which `parser.rs` reads with `include_str!`.

The walk is generic over `nodes.Node.fields` rather than a case per node type, so a node this
script has never heard of still dumps correctly and a field added upstream shows up as a
difference instead of being silently ignored.

Entry point is `Parser(env, src, state="variable").parse_expression()` followed by an
end-of-stream check — that is exactly `Environment.compile_expression`, which is the call
ansible-core's `when:` goes through (`_engine.py:379`).

    python scripts/jinja_ast.py <jinja2-sdist>/tests demo \\
        > crates/ansible-core/src/jinja/parser_corpus.jsonl
"""

import json
import pathlib
import sys
import traceback

from jinja2 import Environment, nodes
from jinja2.parser import Parser

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from jinja_tokens import ADVERSARIAL, harvest  # noqa: E402

env = Environment()

# `ctx` is `load` everywhere reachable from an expression, and a field with one possible value
# is noise in a diff. Everything else upstream declares is compared.
SKIP_FIELDS = {"ctx"}

# Nodes that only exist once code generation runs. None is reachable from source text, so one
# turning up means the corpus wandered somewhere this port does not model.
UNREACHABLE = {
    "NSRef",
    "InternalName",
    "EnvironmentAttribute",
    "ExtensionAttribute",
    "ImportedName",
    "ContextReference",
    "DerivedContextReference",
    "MarkSafe",
    "MarkSafeIfAutoescape",
    "TemplateData",
}


def dump(v):
    if isinstance(v, nodes.Node):
        name = type(v).__name__
        if name in UNREACHABLE:
            raise ValueError(f"unreachable node {name}")
        out = {"t": name}
        for f in v.fields:
            if f in SKIP_FIELDS:
                continue
            out[f] = dump(getattr(v, f))
        return out
    if isinstance(v, (list, tuple)):
        return [dump(x) for x in v]
    if isinstance(v, float) and (v != v or v in (float("inf"), float("-inf"))):
        raise ValueError("non-finite float")
    if isinstance(v, str):
        try:
            v.encode("utf-8")
        except UnicodeEncodeError:
            # A Python string may hold a lone surrogate; a Rust `String` may not, and neither
            # may JSON that Rust will read. ansible-core's own tests contain one —
            # `test/integration/targets/json-serialization/test.yml:6` has
            # `'"hi \udc80 mom"' | from_json`. The parser refuses it by design (see
            # `a_lone_surrogate_is_refused_where_python_keeps_it`), so there is nothing to
            # compare; recording it as a skip keeps that visible rather than emitting a row
            # that cannot be transported.
            raise ValueError("lone surrogate in a string constant") from None
    return v


def cause(exc, parser):
    """Which stage gave up, decided structurally rather than by reading the message.

    Upstream raises `TemplateSyntaxError` from both stages, so the text cannot tell them
    apart -- and neither can the filename: `TokenStream.expect` is defined in `lexer.py`, so
    every *parser* refusal has a `lexer.py` frame too. The tokeniser proper is `tokeniter`
    and `wrap`, so the frame *function* is what separates them.
    """
    frames = traceback.extract_tb(exc.__traceback__)
    if any(f.name in ("tokeniter", "wrap") for f in frames):
        return "lex"
    try:
        return "eof" if parser.stream.current.type == "eof" else "parse"
    except Exception:  # noqa: BLE001 - the stream itself is what blew up
        return "lex"


def row(src):
    src = src.replace("\r\n", "\n").replace("\r", "\n")
    p = None
    try:
        p = Parser(env, src, state="variable")
        tree = p.parse_expression()
        # `compile_expression` refuses anything left over rather than returning a shorter
        # answer. Without this, `foo bar` would be recorded as a tree for `foo`.
        if not p.stream.eos:
            return {"src": src, "err": "chunk after expression", "cause": "trailing"}
        return {"src": src, "ast": dump(tree)}
    except ValueError as e:
        return {"src": src, "skip": str(e)}
    except Exception as e:  # noqa: BLE001 - any refusal is a refusal
        return {"src": src, "err": str(e).splitlines()[0], "cause": cause(e, p)}


def main():
    corpus = list(ADVERSARIAL) + [
        # Shapes the tokeniser could not distinguish but the parser must. Each was run against
        # jinja2 3.1.6 before it went in here.
        "-x ** 2",
        "x ** -2",
        "+1",
        "+x",
        "+x | d",
        "-+x",
        "+-x",
        "not a in b",
        "a not in b",
        "1 < 2 < 3 > 0",
        "a if b else c if d else e",
        "'x' if c",
        "xs[1:2]",
        "xs[::2]",
        "xs[:]",
        "xs[1:2, 3]",
        "f(*a, **k)",
        "f(a, b=1)",
        "f(a,)",
        # `parse_call_args`' ordering rules, one input per `ensure()` upstream.
        "f(**a, **b)",
        "f(**k, a=1)",
        "f(*a, *b)",
        "f(*a, 1)",
        "xs[1:]",
        "xs[1::]",
        "xs[:2]",
        "x | ansible.builtin.default('v')",
        "x is ansible.builtin.defined",
        "x is divisibleby 3",
        "x is sameas foo.bar",
        "x is defined and y is not defined",
        "a.0",
        "a.b.c.d",
        "hostvars[h].x",
        "hostvars['h']['x']",
        "{'a': 1}['a']",
        "('a' ~ 'b' ~ 'c')",
        "()",
        "(1,)",
        "(1, 2)",
        "1, 2",
        "[1, 2,]",
        "{1: 2, 3: 4}",
        "foo bar",
        "x ==",
        "not",
        "foo.",
        "x is",
        "x is not",
        "f(a=1, 2)",
        "f(**k, *a)",
        # Truncated at every nesting depth, so the error path out of each recursive call is
        # taken at least once rather than only the happy path through it. Both sides refuse
        # all of these; what is being checked is that we refuse the same set.
        "(1 +",
        "(1,",
        "[1 +",
        "[1,",
        "{'a': 1 +",
        "{'a'",
        "{'a':",
        "f(1 +",
        "f(*",
        "f(**",
        "f(a=",
        "x[1 +",
        "x[",
        "x[1:",
        "x[1:2:",
        "x[:",
        "a if b else",
        "a if",
        "not (",
        "a or",
        "a and",
        "a ~",
        "a *",
        "a **",
        "a //",
        "a +",
        "a <",
        "a in",
        "a not in",
        "-",
        "+",
        "x |",
        "x | default(",
        "x is defined and",
        "x is not",
        "x.",
        "x['a'",
        "'\\u26'",
        "'\\N{NOPE}'",
        "99999999999999999999999999",
        "x[99999999999999999999999999]",
        "x.99999999999999999999999999",
        # A missing separator rather than a missing operand, so the failure is an `expect`
        # inside a container instead of a recursive parse giving up.
        "(1 2)",
        "f(1 2)",
        "x[1 2]",
        "[1 2]",
        "{'a': 1 'b': 2}",
        "x.y(1",
        # A call applied to the result of a filter or a test, which is a different
        # branch from a call applied to a name: `parse_filter_expr` handles it, not
        # `parse_postfix`.
        "x|d(1)(2)",
        "x|d(1)(2",
        "x is defined(1",
        # A dotted filter or test name whose tail is not a name.
        "x | a.1",
        "x is a.1",
        "x is divisibleby(",
        "x is sameas foo.",
        # The escape refusal has to survive being the *second* of two adjacent strings.
        "'a' '\\u26'",
    ]
    for root in sys.argv[1:]:
        corpus += harvest(pathlib.Path(root).rglob("*"))

    seen, rows, skipped = set(), [], {}
    for src in corpus:
        if src in seen:
            continue
        seen.add(src)
        r = row(src)
        if "skip" in r:
            skipped[r["skip"]] = skipped.get(r["skip"], 0) + 1
            continue
        rows.append(r)

    # Never drop silently: a corpus that quietly discards what it cannot map reads as full
    # coverage while testing whatever is left.
    print(f"{len(rows)} rows, {sum(skipped.values())} skipped {skipped}", file=sys.stderr)

    rows.sort(key=lambda r: r["src"])
    for r in rows:
        print(json.dumps(r, ensure_ascii=True, sort_keys=True))


if __name__ == "__main__":
    main()
