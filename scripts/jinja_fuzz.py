"""Random differential inputs for `jinja::parser`, in `jinja_ast.py`'s format.

A curated corpus only contains what someone thought to write down. This generates expressions
nobody would think of — random trees printed back to source, then mutated into malformed
input — and records what jinja2 3.1.6 makes of each. The Rust side runs the result through
the same comparison as the checked-in corpus.

Output is *not* checked in: it is thousands of rows and regenerating differs by seed. Run it,
compare, and fold anything that mismatches into `jinja_ast.py`'s adversarial block as a
permanent case.

    python scripts/jinja_fuzz.py 5000 7 > $SCRATCH/fuzz.jsonl
    JINJA_FUZZ_CORPUS=$SCRATCH/fuzz.jsonl \\
        cargo test -p ansible-core --lib fuzz_corpus -- --ignored --nocapture

Nesting is capped well below `MAX_DEPTH`, so a refusal here is never just the depth ceiling —
that divergence is deliberate and has its own tests.
"""

import json
import random
import sys

from jinja_ast import row  # noqa: F401  - same env, same dump, same refusal handling

NAMES = ["x", "foo", "r", "hostvars", "groups", "item", "ansible_facts", "_", "l1", "über"]
FILTERS = ["length", "default", "bool", "list", "d", "ansible.builtin.length", "upper"]
TESTS = ["defined", "undefined", "none", "string", "number", "divisibleby", "sameas"]
BINOPS = ["+", "-", "*", "/", "//", "%", "**", "~", "and", "or"]
CMPOPS = ["==", "!=", ">", ">=", "<", "<=", "in", "not in"]
ATOMS = [
    "1", "0", "42", "1.5", "2e3", "0xff", "0b1010", "1_000",
    "'a'", "'a|b'", "\"d\"", "''", "'%}'",
    "true", "false", "none", "True", "None",
]


def gen(rng, depth):
    """One random expression, as source text."""
    if depth <= 0:
        return rng.choice(ATOMS + NAMES)
    pick = rng.randrange(14)
    sub = lambda: gen(rng, depth - 1)  # noqa: E731
    if pick == 0:
        return rng.choice(ATOMS + NAMES)
    if pick == 1:
        return f"({sub()})"
    if pick == 2:
        return f"{sub()} {rng.choice(BINOPS)} {sub()}"
    if pick == 3:
        return f"{sub()} {rng.choice(CMPOPS)} {sub()}"
    if pick == 4:
        return f"not {sub()}"
    if pick == 5:
        return f"-{sub()}"
    if pick == 6:
        return f"{sub()} | {rng.choice(FILTERS)}"
    if pick == 7:
        args = ", ".join(sub() for _ in range(rng.randrange(3)))
        return f"{sub()} | {rng.choice(FILTERS)}({args})"
    if pick == 8:
        neg = "not " if rng.random() < 0.3 else ""
        return f"{sub()} is {neg}{rng.choice(TESTS)}"
    if pick == 9:
        return f"{rng.choice(NAMES)}.{rng.choice(NAMES)}"
    if pick == 10:
        return f"{rng.choice(NAMES)}[{sub()}]"
    if pick == 11:
        items = ", ".join(sub() for _ in range(rng.randrange(4)))
        return f"[{items}]" if rng.random() < 0.5 else f"({items},)"
    if pick == 12:
        pairs = ", ".join(f"{sub()}: {sub()}" for _ in range(rng.randrange(3)))
        return "{" + pairs + "}"
    return f"{sub()} if {sub()} else {sub()}"


# Characters worth splicing in: every operator, both quotes, and things that are not tokens
# at all, so the lexer's refusal paths get exercised as well as the parser's.
NOISE = list("+-*/%~[](){}<>=.,:;|!@#$?^&\\'\" \t\n") + ["**", "//", "==", "!=", ">=", "<=", "'", '"']


def mutate(rng, s):
    """Damage a valid expression: delete, duplicate, splice, or truncate."""
    if not s:
        return s
    how = rng.randrange(5)
    i = rng.randrange(len(s))
    if how == 0:
        return s[:i] + s[i + 1 :]
    if how == 1:
        return s[:i] + rng.choice(NOISE) + s[i:]
    if how == 2:
        return s[:i]
    if how == 3:
        j = rng.randrange(len(s))
        return s[:i] + s[j:]
    return s + rng.choice(NOISE)


def main():
    count = int(sys.argv[1]) if len(sys.argv) > 1 else 2000
    depth = int(sys.argv[2]) if len(sys.argv) > 2 else 6
    # Fixed seed: a failure has to be reproducible, and "it passed last time" is not a result.
    rng = random.Random(int(sys.argv[3]) if len(sys.argv) > 3 else 20260828)

    seen, rows, skipped = set(), [], 0
    while len(rows) < count:
        src = gen(rng, rng.randrange(1, depth + 1))
        # Half the corpus is broken on purpose; a parser is judged by what it refuses.
        if rng.random() < 0.5:
            for _ in range(rng.randrange(1, 4)):
                src = mutate(rng, src)
        if src in seen:
            continue
        seen.add(src)
        r = row(src)
        if "skip" in r:
            skipped += 1
            continue
        rows.append(r)

    ok = sum(1 for r in rows if "ast" in r)
    print(
        f"{len(rows)} rows: {ok} parse, {len(rows) - ok} refused, {skipped} skipped",
        file=sys.stderr,
    )
    for r in rows:
        print(json.dumps(r, ensure_ascii=True, sort_keys=True))


if __name__ == "__main__":
    main()
