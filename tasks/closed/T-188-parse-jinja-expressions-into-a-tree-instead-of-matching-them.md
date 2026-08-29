# T-188 — Parse Jinja expressions into a tree instead of matching them as strings

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P2       | L    | T-114 | T-213      |

## Problem

`condition::classify` recognises shapes by cutting strings apart:

```rust
s.split_once(" is ")                       // `x is defined`
s.strip_suffix("> 0")                      // `... > 0`
   .and_then(|l| l.strip_suffix("| length"))
```

Every shape is written against one exact spelling of a construct Jinja will accept in many.
Two bugs already filed are the same defect seen from different sides:

- **T-186** — ~~`r.stdout | length > 0` classifies as `RequiresNonEmpty { var: "r" }`~~
  **fixed**, and not at the string level: `Verdict` now carries `VarRef { root, expr }` and
  `parse_var_ref` reads a root plus `.ident` / literal `[...]` steps. That half of the work
  below is done and survives this ticket — the tree replaces *how* the reference is found, not
  the type that holds it. What remains here is everything the accessor parser does not cover:
  filters as structure, comparisons, boolean nesting. A non-literal subscript
  (`hostvars[h].x`) is refused today and stays refused until the tree can represent it.
- **T-187** — `hosts|length>0` classifies as `Unknown` while `hosts | length > 0` classifies
  fine. Identical meaning, different answer, because the matchers only ever see the spaced
  spelling.

Neither is fixable at the string level without adding another special case per spelling. The
root cause is that `r.stdout` is an **expression** — an accessor applied to the variable `r` —
and a `String` cannot represent that. Patching the label or padding the operators treats the
symptom; the next spelling breaks it again.

## Approach

A small expression tree, and `classify` reads the tree instead of the text. Enough of Jinja to
answer the questions the rules actually ask, not a reimplementation:

- **names and accessors** — `r`, `r.stdout`, `r['stdout']`, `r.results[0].stdout`, so the root
  variable and the accessed path are separately available. This is what T-186 needs.
- **filter chains with arguments** — `| length`, `| default('x')`, `| default([], true)`, so a
  shape is recognised by structure rather than by suffix.
- **comparisons and boolean structure** — `> 0`, `== 'x'`, `in [...]`, `and`/`or`/`not`.
- **tests** — `is defined`, `is not defined`, `is skipped`.

Whitespace stops mattering by construction, so T-187 dissolves rather than being fixed.

**Non-goals.** Evaluation is T-035, not this. Statements (`{% for %}`, `{% if %}`, macros) are
not expressions and are out of scope. Full Jinja2 grammar conformance is not the bar — the bar
is that anything not represented parses to a node that classifies as `Unknown`, never a guess.

**Also unblocks**, and worth checking before designing the tree so it serves them too:

- `resolve::substitute_literals` currently requires a bare identifier and abandons the whole
  value on any filter, which is what makes `{{ x | default('lit') }}` fall through to globbing
  (recorded with measurements on T-034)
- T-089 indexed access into static list vars
- T-115's filter index has a natural home once filters are nodes rather than substrings
- T-040, and more directly than the others: `parse_extends`, `parse_include`, `parse_import`
  and `parse_from` each read their target with `node.template = self.parse_expression()`
  (jinja2 3.1.6). The expression parser *is* the thing that reads an include target, so T-040
  gets that half for free. It still owns the rest of the grammar in full — the five lexer
  states and all 14 statement forms — because a `.j2` file goes down Ansible's *template*
  compile path and a reader that skips unmodelled tags can never report a template that will
  not render. The split is not a subset of one grammar; it is Ansible's two compile paths.

### The boundary is Ansible's, not ours

`_engine.py:293` routes a value to one of two entry points. `when:`, `loop:` and `assert.that`
take `_compile_expression` (`playbook/task.py:615`, `executor/task_executor.py:178`,
`plugins/action/assert.py:81`), which is `env.compile_expression` — `Parser(state="variable")`,
`parse_expression()`, and nothing else. **This surface physically cannot contain a statement**,
so "expressions only" is complete here rather than a subset of something larger. Everything
else goes to `_compile_template` and is T-040's.

That same function ends with `if not parser.stream.eos: raise TemplateSyntaxError("chunk after
expression")`, which is the error `expressions.rs` already quotes. It is not decoration: a bare
`parse_expression()` **stops at the first complete expression and leaves the rest**. Measured on
jinja2 3.1.6 — `x y` returns `Name('x')` with `name:'y'` still on the stream, and `x }} y`
returns `Name('x')` with `variable_end` pending. Porting the call without porting the check
would make `when: foo bar` classify as a confident claim about `foo`, which is this ticket's
own failure mode reintroduced by the fix for it.

(ansible-core lines read from the 2.21.3 sdist and **not run** — no core installed on the
machine this was written on. The jinja2 lines were run.)

## Scope: which readers the tree reaches

`classify` is the only consumer migrated. Five others in the same file still scan expression
text and are deliberately left alone:

| reader | line | why it stays on text |
| --- | --- | --- |
| `variable_uses` | `condition.rs:440` | wants every name *and its byte span*, for spans in diagnostics |
| `any_uses` | `condition.rs:458` | same, without the not-a-variable filter |
| `hostvars_uses` | `condition.rs:475` | same |
| `hostvars_host_keys` | `condition.rs:533` | runs on arbitrary text, not on a condition — often not a parseable expression at all |
| `scan_words` | `condition.rs:728` | the shared word scanner the four above sit on |

This is a knowing violation of rule 3 — two notions of what a reference is, one file apart —
and it is recorded here rather than left implicit.

It is *not* for want of spans: `Expr` carries a `span` on every node, so the tree could place
a squiggle today. The real difference is the input contract. `classify` is handed one
condition and may answer `Unknown`, so a parse failure is a legal outcome. The scanners are
handed text with no such guarantee — `hostvars_host_keys` takes arbitrary strings, and all
four report *every* name they find in input that may never parse at all. Routing them through
a parser trades "finds the names" for "finds the names, unless the line has a typo in it",
which is a behaviour change for hover and go-to-definition and belongs in its own ticket with
its own corpus measurement — not smuggled in behind this one.

## Done when

- [x] an expression tree that distinguishes the root variable from the accessor path, with
      `r`, `r.stdout`, `r['stdout']` and `r.results[0].stdout` each asserted — done under
      T-186 as `VarRef` + `parse_var_ref`, not as a tree. Keep it: the tree feeds this type
      rather than replacing it
- [x] `classify` reads the tree; no `strip_suffix`/`split_once` shape matching remains in it
- [x] T-186's case is correct — never a claim about `r` — asserted on both `label()` and
      `requirement()` by `a_dotted_path_is_named_in_full_never_reduced_to_its_root`, which
      covers all seven `var`-carrying arms
- [x] T-187's case classifies identically to its spaced spelling, plus one non-`RequiresNonEmpty`
      arm asserted the same way
- [x] a quoted operator survives: `x == "a|b"` keeps its literal value
- [x] the parse is required to reach end of stream, with `x y` and `x }} y` asserted `Unknown`
      rather than a claim about `x` — Ansible's own "chunk after expression" check
- [x] anything unrepresentable is `Unknown`, with a test naming a construct we deliberately do
      not model
- [x] refusals are differential on more than "both said no": every one of the 476 refusal rows
      carries the *stage* jinja2 gave up at, and ours must match. 472 agree exactly. The 4 that
      do not are one cause — this port tokenises eagerly, jinja2 lazily, so a lexical fault
      after the point jinja2 stopped is found here and not there. That can only move a refusal
      into `Cause::Lex`, which is what the test asserts, with the count pinned at 4.
- [x] the T-032 corpus classification rate is re-measured and recorded. **It rose.**

      The 39% / 93% figures came from `~/app/ansible` via `condition::corpus::when_coverage`,
      which no longer exists in the tree and whose corpus is not on this machine — so they are
      not reproducible and are not what this compares against. `when_coverage` is restored as
      an `ANSIBLE_CORPUS`-gated test and run over the eight public trees T-184 pinned, with the
      *same* sweep run against the pre-migration `classify` for the before column:

      | tree | commit | before | after | Δ |
      | --- | --- | ---: | ---: | ---: |
      | `debops/debops` | `65b66ff` | 204 | 355 | **+151** |
      | `ansible-collections/community.general` | `0bf15b1` | 367 | 372 | +5 |
      | `kubernetes-sigs/kubespray` | `46dbdd3` | 368 | 373 | +5 |
      | `geerlingguy/ansible-role-mysql` | `0a0ea6b` | 23 | 26 | +3 |
      | `openstack/openstack-ansible` | `3dcf546` | 29 | 30 | +1 |
      | `ansible/ansible-examples` | `b505865` | 7 | 7 | 0 |
      | `sovereign/sovereign` | `9fd5ff5` | 6 | 6 | 0 |
      | `ansible/ansible` | `b85437b` | 537 | 528 | **−9** |
      | **total** | | **1541** (13%) | **1697** (14%) | **+156** |

      11 379 condition sites, 22 180 clauses, 7 334 files. debops carries the gain almost
      alone, exactly as T-211 predicted from its 852 uses of `d(` — which is the argument for
      quoting these per-repo rather than blended: the same change is +74% for one tree and 0%
      for two others.

      **The −9 is the fix working, not a regression**, and each shape is pinned by
      `shapes_the_string_matcher_claimed_and_could_not_have_known`:

      - `1 in [1,2,3]`, `200 is not defined` — a literal is not a variable. The old answer was
        `WhenIn { var: "1", values: ["1", "2", "3"] }`: a claim about a variable named `1`.
      - `ansible_distribution in ('RedHat')` — a parenthesised string is a string, so this is a
        **substring** test. Live on ansible-core 2.21.2: `distro` of `Red` runs it and so does
        `Hat`, while the control `distro in ["RedHat"]` skips for `Red`. The old verdict
        "runs when it is one of: RedHat" was false for every substring.
      - `result.url|default("") == "https://" + httpbin_host + "/get"` — the right-hand side is
        a concatenation. The old matcher split on `==` and reported the raw text, `+` signs and
        all, as the literal value.
- [x] T-114's standing box honoured: every verdict this change newly produces on the corpus was
      run against real ansible-core rather than read.

      Each of the 65 single-verdict conditions the tree gained was turned into a falsifiable
      prediction — `OnlyIfSet` means unset must skip, `WhenIn` means a member must run and an
      outsider must not, and so on — giving **128 predictions**, run as one playbook on
      ansible-core 2.21.2. **126 matched.**

      Two did not, and both are the same finding: the harness fed a *string* where the source
      wrote an *integer*, which it could only do because the verdict does not record which it
      was. The control settles it — with `rc` as the integer, ansible does exactly what the
      verdict claims. Filed as **T-214**; no verdict is semantically wrong.

      The 23 conditions whose verdict *changed* were read individually. All 23 are the old
      matcher having been wrong, the worst being `list_literals` silently dropping every list
      entry containing a space: `["Flatcar", "Flatcar Container Linux by Kinvolk"]` was
      reported as `["Flatcar"]`, and `openSUSE Leap` / `openSUSE Tumbleweed` / `""` likewise
      vanished. That was a wrong hover shipped on kubespray, fixed by this change.

      **One genuine regression was found and is not fixed here: T-213.** Widening reach to the
      `d(` spelling made three debops conditions classify as `WhenIn` where the default value
      is itself in the list — so the hint says "runs only if …" for a task that runs by
      default. A silence became a wrong answer. T-213 should close before this ticket does.
