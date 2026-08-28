"""Generate the golden token streams `jinja::lexer` is checked against.

The lexer is a port of jinja2 3.1.6's `tag_rules`, so the only honest test of it is the
thing it was ported from. This runs jinja2's own tokeniser over a corpus and writes one
JSON object per line; `lexer.rs` reads the result with `include_str!` and asserts equality.

The corpus is not invented here. It is every `{{ ... }}` and `{% ... %}` body that appears
in jinja2's own test suite plus this repo's `demo/`, deduped, with a hand-written block of
adversarial cases appended. `block_begin` and `variable_begin` share `tag_rules`, so a
statement body exercises exactly the same scanner as an expression.

    python scripts/jinja_tokens.py <jinja2-sdist-tests-dir> > crates/ansible-core/src/jinja/lexer_corpus.jsonl

Regenerating needs the sdist, not the wheel — the wheel ships no tests:

    pip download jinja2==3.1.6 --no-binary :all: --no-deps -d /tmp/j2 && tar xzf /tmp/j2/*.tar.gz -C /tmp/j2
"""

import json
import pathlib
import re
import sys

from jinja2 import Environment

env = Environment()

# jinja2's token type names -> our `Kind` variants. Whitespace is dropped by both.
KIND = {
    "name": "Name",
    "string": "Str",
    "integer": "Int",
    "float": "Float",
    "add": "Add",
    "sub": "Sub",
    "mul": "Mul",
    "div": "Div",
    "floordiv": "FloorDiv",
    "mod": "Mod",
    "pow": "Pow",
    "tilde": "Tilde",
    "eq": "Eq",
    "ne": "Ne",
    "gt": "Gt",
    "gteq": "Gteq",
    "lt": "Lt",
    "lteq": "Lteq",
    "assign": "Assign",
    "dot": "Dot",
    "comma": "Comma",
    "colon": "Colon",
    "semicolon": "Semicolon",
    "pipe": "Pipe",
    "lparen": "Lparen",
    "rparen": "Rparen",
    "lbracket": "Lbracket",
    "rbracket": "Rbracket",
    "lbrace": "Lbrace",
    "rbrace": "Rbrace",
}

# `tokeniter` is the raw scanner: it emits one `operator` token carrying the matched text, and
# it is `wrap()` that splits that into `pow`, `dot` and the rest. Reading only `KIND` above
# therefore drops every expression containing an operator — which is nearly all of them, and
# which is exactly what this table exists to stop.
OPERATOR_KIND = {
    "+": "Add",
    "-": "Sub",
    "/": "Div",
    "//": "FloorDiv",
    "*": "Mul",
    "%": "Mod",
    "**": "Pow",
    "~": "Tilde",
    "[": "Lbracket",
    "]": "Rbracket",
    "(": "Lparen",
    ")": "Rparen",
    "{": "Lbrace",
    "}": "Rbrace",
    "==": "Eq",
    "!=": "Ne",
    ">": "Gt",
    ">=": "Gteq",
    "<": "Lt",
    "<=": "Lteq",
    "=": "Assign",
    ".": "Dot",
    ":": "Colon",
    "|": "Pipe",
    ",": "Comma",
    ";": "Semicolon",
}

# Deliberately broken, and a few shapes the corpus is unlikely to contain. Every one of these
# was checked against jinja2 3.1.6 by hand before being put here.
ADVERSARIAL = [
    "!x",
    "a@b",
    "a#b",
    "a$b",
    "'unterminated",
    '"also unterminated',
    "\U0001f40da",
    "a\U0001f40d",
    "·",
    "a·",
    "007",
    "01",
    "0b_1",
    "0x_",
    "1__0",
    "1_000_",
    "foo.5e3",
    "foo.5.5",
    "1.5e",
    "1.e3",
    ".5",
    "0b2",
    "'a|b'",
    "'%}'",
    "'{{'",
    "'it\\'s'",
    "'a\nb'",
    "'a\r\nb'",
    "hosts|length>0",
    "hosts | length > 0",
    "r.stdout | length > 0",
    "not (skip_x | default(false) | bool)",
    "r.results[0].stdout",
    "mode | default('native') == 'native'",
    "x is not defined",
    "xs[1:2:-1]",
    "{'k': [1, 2]}",
    "a if b else c",
    "1 < 2 < 3",
    "2**3**2",
    "a >= b",
    "a <= b",
    "a ~ b",
    "a; b",
    "a = b",
    "a // b",
    "a != b",
    "-x ** 2",
    "xs | map(attribute='x') | list",
    "",
    "   ",
    "}}",
    "{%",
]

BODY = re.compile(r"\{\{(.*?)\}\}|\{%-?(.*?)-?%\}", re.S)


def harvest(paths):
    seen = []
    for p in paths:
        try:
            text = p.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for a, b in BODY.findall(text):
            body = (a or b).strip()
            if body:
                seen.append(body)
    return seen


def row(src):
    # `tokeniter` rewrites the source before scanning — `newline_re.split(source)[::2]`
    # rejoined with "\n" — so a CRLF shifts every offset after it and spans stop lining up
    # with the file on disk. Normalising here keeps the corpus comparable; the Rust lexer
    # deliberately does *not* rewrite its input, because an LSP span has to point into the
    # document the user is looking at. The CR cases are pinned by hand in `lexer.rs` instead.
    src = src.replace("\r\n", "\n").replace("\r", "\n")
    try:
        pieces = list(env.lexer.tokeniter(src, None, None, "variable"))
    except Exception as e:  # noqa: BLE001 - any refusal is a refusal
        msg = str(e).splitlines()[0]
        at = re.search(r" at (\d+)$", msg)
        # Upstream reports a codepoint index; convert it to a byte index the same way.
        pos = len(src[: int(at.group(1))].encode("utf-8")) if at else None
        return {"src": src, "err_at": pos}

    # `tokeniter` yields the raw matched text in order, so concatenating it reproduces the
    # source — which is how a byte span is recovered from a stream that only tracks linenos.
    # Byte offsets, not character offsets. Python indexes strings by codepoint, so every
    # token after a multi-byte literal would be reported two bytes early for `'a·'`.
    # A Rust `Span` is a byte range into the file, which is also what LSP positions need.
    out, pos = [], 0
    for _lineno, kind, value in pieces:
        start, pos = pos, pos + len(value.encode("utf-8"))
        if kind == "whitespace":
            continue
        if kind == "operator":
            if value not in OPERATOR_KIND:
                return {"src": src, "skip": f"operator {value!r}"}
            out.append([OPERATOR_KIND[value], start, pos])
            continue
        if kind not in KIND:
            return {"src": src, "skip": kind}
        # `tokeniter` is the raw scanner; the identifier check is in `wrap()`, which this does
        # not go through. Without it a lone `·` comes back as a perfectly good name, because
        # the generated `name_re` matches it and only `str.isidentifier()` objects.
        # No offset: upstream raises this one with a lineno and no column.
        if kind == "name" and not value.isidentifier():
            return {"src": src, "err_at": None}
        out.append([KIND[kind], start, pos])
    assert pos == len(src.encode("utf-8")), f"token text did not reproduce {src!r}"
    return {"src": src, "toks": out}


def main():
    corpus = list(ADVERSARIAL)
    for root in sys.argv[1:]:
        corpus += harvest(pathlib.Path(root).rglob("*"))

    seen, rows, skipped = set(), [], {}
    for src in corpus:
        if src in seen:
            continue
        seen.add(src)
        r = row(src)
        if "skip" in r:
            skipped.setdefault(r["skip"], 0)
            skipped[r["skip"]] += 1
            continue
        rows.append(r)

    # A corpus that quietly drops what it cannot map reads as full coverage while testing
    # whatever is left. The first version of this script dropped every expression containing
    # an operator — all of them, silently — so what is skipped is now printed and counted.
    print(f"{len(rows)} rows, {sum(skipped.values())} skipped {skipped}", file=sys.stderr)

    rows.sort(key=lambda r: (r["src"] is None, r["src"]))
    for r in rows:
        print(json.dumps(r, ensure_ascii=True, sort_keys=True))


if __name__ == "__main__":
    main()
