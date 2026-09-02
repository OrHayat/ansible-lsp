//! Semantic tokens for a Jinja source — what the editor paints, decided by the parser
//! rather than by a regex.
//!
//! The client ships a TextMate grammar that paints the same shapes, and it is wrong in the
//! cases a regex cannot reach: it hardcodes `{%`, so a template whose `#jinja2:` header
//! renames the delimiters gets its real tags missed and its literal `{%` painted as a tag —
//! exactly backwards. This runs [`template::document_in`], which reads the header, so the
//! answer follows the delimiters the template actually renders with.
//!
//! Deliberately span-based and delimiter-parameterised rather than tied to a file: the
//! second consumer is Jinja embedded in a YAML scalar, where the source is a slice of a
//! playbook and the delimiters come from the task that renders it.

use super::{lexer, template, Delimiters};
use crate::parse::Span;

/// The token kinds this produces. Named for LSP's standard set so the server's legend is a
/// direct mapping and no client needs to be taught custom names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Comment,
    Keyword,
    Variable,
    Function,
    /// The name after a `.` — `hostname` in `ansible_facts.hostname`.
    ///
    /// Split from [`TokenType::Variable`] because the two resolve differently and a theme
    /// should be able to say so: the root is looked up in the render context, the property
    /// is looked up on whatever the root turned out to be. A called one, `m.upstream(...)`,
    /// stays a [`TokenType::Function`] — the same call as Python's `obj.method()`, which
    /// Pylance types `method`, not `property`.
    Property,
    /// `group` in `{% macro upstream(group) %}` — a macro's argument, which is the one place
    /// LSP's `parameter` is the truth. A loop target is not one; see [`SemToken::declaration`].
    Parameter,
    /// `m` in `{% import "macros.j2" as m %}` — a module-like name whose members are macros.
    /// Standard `namespace`, which is what Pylance sends for `import x as m`.
    Namespace,
    /// `body` in `{% block body %}` — a name two templates agree on so a child can fill the
    /// parent's slot. Not a variable: nothing looks it up and `{{ body }}` is undefined. Not a
    /// function either, whatever Jinja compiles it to. Not standard LSP — the protocol has no
    /// label type and neither does VS Code's registry — so the client maps it to
    /// `entity.name.label`, the scope the bundled C, C#, JavaScript and TypeScript grammars
    /// give a goto label, which Dark+ paints and any theme that colours C labels colours.
    Label,
    String,
    Number,
    /// A symbolic operator — `|`, `==`, `+`. Go and Python both leave these at the default
    /// foreground, and [`TokenType::WordOperator`] is deliberately a different thing.
    Operator,
    /// `and`, `or`, `not`, `in`, `is` — an operator spelled as a word.
    ///
    /// Split from [`TokenType::Keyword`] because the two are not the same thing and the
    /// precedent says so: Python paints `and` `#569cd6` and `return` `#C586C0`, and Go paints
    /// `&&` at the default foreground and `if` `#C586C0`. Calling a tag name and a word
    /// operator the same kind of token is what made `{% if x and true %}` one flat colour.
    WordOperator,
    /// `true`, `false`, `none` and their capitalised spellings — a literal, not a keyword.
    ///
    /// The set is closed: jinja2's `parse_primary` decides it, and `env.parse("{{ X }}")`
    /// answers `Const` for exactly these six and `Name` for `null`, `nil` and `TRUE`. Same
    /// fact as `condition::LITERALS`, and the drift between two copies of it was [[T-220]].
    Constant,
    /// Literal output that the client's grammar paints as something it is not.
    ///
    /// The grammar matches `{# … #}` from a hardcoded `{#`, which is right for every template
    /// that leaves the comment delimiters alone — measured, 0 of 1222 `.j2` files across seven
    /// public trees move any delimiter. On the ones that do, that text is *rendered into the
    /// output file*, verified against ansible-core 2.21.3: with `comment_start_string:"<#"` the
    /// line `LINE1 {# curly comment #} END1` comes out verbatim.
    ///
    /// A semantic token cannot un-paint a grammar scope — [`SparseTokensStore::addSparseTokens`]
    /// merges per attribute and only visits ranges a token covers, so silence leaves the
    /// grammar's colour standing. It can *re*paint, which is what this is for: emitted only
    /// over the ranges the grammar is about to get wrong, mapped to a scope that resolves to
    /// the editor's default foreground so the text reads as the output it is.
    Text,
    /// `{%`, `%}`, `{{`, `}}` — the delimiter itself, whatever it currently is.
    ///
    /// Not a standard LSP type, and not decoration: the client's grammar used to paint these
    /// (`punctuation.definition.tag`, a dimmer grey than ordinary text) and stopped when its
    /// tag rules were removed, because a grammar cannot know which characters are delimiters.
    /// The parser can, so it says so — including on a file whose `#jinja2:` header or render
    /// site renamed them, where the grammar was painting the wrong characters anyway.
    Delimiter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemToken {
    pub span: Span,
    pub ty: TokenType,
    /// This occurrence introduces the name rather than reading it — `h` in
    /// `{% for h in hosts %}`, where `hosts` is looked up and `h` is not looked up anywhere.
    ///
    /// LSP's standard `declaration` modifier, on a [`TokenType::Variable`]. A modifier and not
    /// a type because it *is* a variable — `{{ h }}` in the body is the same kind of thing —
    /// and not `parameter`, which is a function's argument and would be a lie here. A lexical
    /// fact about one line, so unlike the resolvability modifier T-217 leaves open it claims
    /// nothing about the workspace.
    pub declaration: bool,
}

/// Operators the lexer returns as `Name` because they are spelled as words.
const WORD_OPERATORS: &[&str] =
    &["and", "or", "not", "in", "is", "if", "else", "elif", "as", "with", "without", "recursive"];

/// The six spellings jinja2 turns into a literal. Closed set — see [`TokenType::Constant`].
const LITERALS: &[&str] = &["true", "True", "false", "False", "none", "None"];

/// Tokens for a whole template. `root` says whether this source's own `#jinja2:` header
/// applies — false for a partial that only ever gets included, matching
/// [`template::document_in`].
pub fn tokens(src: &str, base: &Delimiters, root: bool) -> Vec<SemToken> {
    // A template that does not split is not a reason to paint nothing: the diagnostic says
    // it will not render, and leaving the file grey on top of that helps nobody. The blocks
    // up to the failure are still the right answer, but `document_in` returns none of them,
    // so an unparseable source simply produces no tokens and the grammar underneath stands.
    let Ok((blocks, delims)) = template::document_in(src, base, root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut known = Known::default();
    // Only worth walking the data when the grammar is about to be wrong. With the default
    // delimiters its `{# … #}` rule agrees with us and a repaint would be pure noise.
    let grammar_is_wrong = delims.comment_start != Delimiters::default().comment_start;
    for b in blocks {
        match b.kind {
            template::Kind::Data => {
                if grammar_is_wrong {
                    repaint_literal_comments(src, b.span, &mut out);
                }
            }
            template::Kind::Comment => {
                out.push(SemToken { span: b.span, ty: TokenType::Comment, declaration: false })
            }
            template::Kind::Statement | template::Kind::Expression => {
                // `span` covers the delimiters and `inner` only what is between them, so the
                // two edges are exactly the opening and closing delimiter. Emitting them is
                // what a grammar used to do from a hardcoded `{%`; here they follow whatever
                // the file actually renders with.
                // In source order, and that is load-bearing: the protocol encodes each
                // token as a delta from the previous one, so an out-of-order push underflows
                // the column subtraction. Emitting both delimiters before the content did
                // exactly that, and the encoder panicked rather than mis-painting.
                delimiter(b.span.start, b.inner.start, &mut out);
                inner_tokens(src, b.inner, b.kind == template::Kind::Statement, &mut known, &mut out);
                delimiter(b.inner.end, b.span.end, &mut out);
            }
        }
    }
    debug_assert!(
        out.windows(2).all(|w| w[0].span.start <= w[1].span.start),
        "tokens must be in source order — the LSP encoding is deltas and underflows otherwise"
    );
    out
}

/// Tokens for a **bare** Jinja expression — a `when:` clause, where the whole scalar is the
/// expression and there are no delimiters to anchor to.
///
/// Ansible wraps a `when:` in `{{ }}` itself before evaluating, which is why
/// [`crate::vars::uses`] already reads one as an expression rather than as literal text with
/// `{{ }}` islands. Painting it the same way keeps one rule for what a `when:` *is*; treating
/// it as a template would paint nothing at all, since there is no `{{` in `foo is defined`.
///
/// `is_statement` is false: the first name in a `when:` is a variable, not a tag keyword.
pub fn expression_tokens(src: &str) -> Vec<SemToken> {
    let mut out = Vec::new();
    // One expression has no statements before it, so nothing is known.
    inner_tokens(src, Span { start: 0, end: src.len() }, false, &mut Known::default(), &mut out);
    out
}

/// What the statements walked so far introduced, by name — the memory across statements
/// that makes `m` in `{{ m.upstream('web') }}` the namespace an earlier `{% import %}` made
/// it, rather than a variable that happens to share the spelling.
///
/// Source order is the scope rule, which is Jinja's own: an import or macro is visible
/// from its statement to the end of the file, and a use *before* it is an ordinary lookup
/// that would fail at render. A later declaration of the same name under another kind —
/// `{% for m in … %}` after the import — replaces the entry, so the loop's `m` reads as the
/// variable it now is.
///
/// Only namespaces and functions are remembered. A parameter is scoped to its macro body
/// and a variable is already a variable, so neither changes a later token's answer.
#[derive(Default)]
struct Known(std::collections::HashMap<String, TokenType>);

impl Known {
    fn declare(&mut self, name: &str, ty: TokenType) {
        self.0.insert(name.to_string(), ty);
    }

    /// The kind a bare use of `name` should paint as, if a statement before it said so.
    fn use_of(&self, name: &str) -> Option<TokenType> {
        self.0.get(name).copied().filter(|t| matches!(t, TokenType::Namespace | TokenType::Function))
    }
}

/// Repaint the `{# … #}` runs inside one data block.
///
/// These are literal output on this template — the real comment delimiter is something else —
/// but the grammar cannot know that and paints them as comments. Emitting a token over exactly
/// the range it mis-paints is the only way to take the colour back.
///
/// Unterminated `{#` is left alone: the grammar's `begin`/`end` would run its comment to the
/// end of the file, and painting a span we cannot bound is guessing in the other direction.
fn repaint_literal_comments(src: &str, span: Span, out: &mut Vec<SemToken>) {
    let d = Delimiters::default();
    let text = span.slice(src);
    let mut i = 0;
    while let Some(rel) = text[i..].find(&d.comment_start) {
        let start = i + rel;
        let after = start + d.comment_start.len();
        let Some(erel) = text[after..].find(&d.comment_end) else { break };
        let end = after + erel + d.comment_end.len();
        out.push(SemToken {
            span: Span { start: span.start + start, end: span.start + end },
            ty: TokenType::Text,
            declaration: false,
        });
        i = end;
    }
}

/// Is the name at `i` the test in an `is` expression?
///
/// `is` may be followed by `not` before the test name (`x is not defined`), and the name may
/// be dotted. A literal after `is` stays a literal — `x is none` is a test spelled as one of
/// the six constants, and jinja2 resolves it as a test, but calling it a function here would
/// contradict [`TokenType::Constant`] for the identical token elsewhere. Left as a constant
/// on purpose; the ambiguity is jinja2's, not ours to invent an answer for.
fn is_test_name(toks: &[lexer::Token], i: usize, text: &str) -> bool {
    let word_before = |j: usize| toks.get(j).map(|t| t.span.slice(text));
    match i.checked_sub(1).and_then(word_before) {
        Some("is") => true,
        Some("not") => i.checked_sub(2).and_then(word_before) == Some("is"),
        _ => false,
    }
}

/// One delimiter — an edge of `span` outside `inner` — if it is not empty.
///
/// Skips an empty edge rather than emitting a zero-width token: a client is entitled to treat
/// `length: 0` as malformed, and there is nothing to paint.
fn delimiter(start: usize, end: usize, out: &mut Vec<SemToken>) {
    if end > start {
        out.push(SemToken { span: Span { start, end }, ty: TokenType::Delimiter, declaration: false });
    }
}

/// Classify the contents of one `{% … %}` or `{{ … }}`.
///
/// `is_statement` marks the first name as the tag keyword — `include` in `{% include x %}`
/// is a keyword, while the identical text in `{{ include }}` is a variable.
fn inner_tokens(
    src: &str,
    inner: Span,
    is_statement: bool,
    known: &mut Known,
    out: &mut Vec<SemToken>,
) {
    let text = inner.slice(src);
    // An unlexable body is left to the grammar rather than half-painted.
    let Ok(toks) = lexer::tokens(text) else { return };
    let at = |s: Span| Span { start: inner.start + s.start, end: inner.start + s.end };

    let shape = if is_statement { Shape::of(&toks, text) } else { Shape::Other };

    for (i, t) in toks.iter().enumerate() {
        let (ty, declaration) = match t.kind {
            lexer::Kind::Eof => continue,
            lexer::Kind::Str => (TokenType::String, false),
            lexer::Kind::Int | lexer::Kind::Float => (TokenType::Number, false),
            lexer::Kind::Name => {
                let word = t.span.slice(text);
                let prev = i.checked_sub(1).map(|j| toks[j].kind);
                let next = toks.get(i + 1).map(|n| n.kind);
                if is_statement && i == 0 {
                    // The tag name. `include` here is a keyword; the identical text in
                    // `{{ include }}` is a variable.
                    (TokenType::Keyword, false)
                } else if let Some(introduced) = shape.introduces(&toks, i, text) {
                    // Before the generic arms: `upstream(` in `{% macro upstream(group) %}`
                    // is being *defined*, which the call arm below cannot see, and `group`
                    // after its `(` is a parameter, which the fallback would call a variable.
                    introduced
                } else if next == Some(lexer::Kind::Assign)
                    && matches!(prev, Some(lexer::Kind::Lparen | lexer::Kind::Comma))
                {
                    // `to_nice_yaml(indent=2, width=80)`: a keyword argument names the
                    // callee's parameter. Not a variable being read — it was the single
                    // most-painted "variable" in kubespray — and not a declaration either,
                    // which is what separates it from the macro arm above: VS Code's own
                    // Python grammar draws the same line, `variable.parameter.function`
                    // at a `def` and `variable.parameter.function-call` at a call.
                    (TokenType::Parameter, false)
                } else if LITERALS.contains(&word) {
                    (TokenType::Constant, false)
                } else if WORD_OPERATORS.contains(&word) {
                    (TokenType::WordOperator, false)
                } else if is_test_name(&toks, i, text) {
                    // `x is defined`, `x is not none`, `x is sameas y` — the name after `is`
                    // is a test, which is the same shape as a filter after `|` and was being
                    // called a variable.
                    (TokenType::Function, false)
                } else if prev == Some(lexer::Kind::Pipe) || next == Some(lexer::Kind::Lparen) {
                    // A filter after `|`, or anything being called. Both read as functions,
                    // and both are wrong to call a variable.
                    (TokenType::Function, false)
                } else if prev == Some(lexer::Kind::Dot) {
                    // After the call check on purpose: `m.upstream(` is a call first and a
                    // property second, and one token has one colour.
                    (TokenType::Property, false)
                } else if let Some(introduced_as) = known.use_of(word) {
                    // A bare name an earlier statement introduced: the `m` of
                    // `{{ m.upstream('web') }}` is the namespace its import made it.
                    (introduced_as, false)
                } else {
                    (TokenType::Variable, false)
                }
            }
            _ => (TokenType::Operator, false),
        };
        if declaration {
            known.declare(t.span.slice(text), ty);
        }
        out.push(SemToken { span: at(t.span), ty, declaration });
    }
}

/// What a statement's tag name says about the names after it — the statements that
/// *introduce* a name rather than read one.
///
/// Everything here is decided from the one statement's own tokens. The other half of the
/// question — that `m` in a later `{{ m.upstream('web') }}` is the namespace this file's
/// `{% import %}` introduced — needs memory across statements, which this classifier does
/// not have and T-217 leaves open.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// `for k, v in d.items() if k in wanted`: every name before the first `in` is a
    /// binding. Past it — the iterable, the filter, the filter's own `in` — is a read.
    For { in_at: Option<usize> },
    /// `macro name(a, b=1)`: `name` is a function being defined and the names directly after
    /// `(` or `,` inside its parens are parameters — `b`, not the `1` it defaults to.
    Macro { parens: Option<(usize, usize)> },
    /// `import "x" as m`: the name after `as` is a namespace being introduced.
    Import,
    /// `from "x" import a, b as c with context`: every name after `import` is a macro of
    /// the other file. The one that lands in *this* scope — `a`, and `c` rather than `b` —
    /// is being introduced. `with`/`without context` ends the list.
    From { names: Option<(usize, usize)> },
    /// `set a, b = …`, `set x | upper`, `set ns.attr = …`: the names before `=` or `|` are
    /// being introduced, unless after a dot — that is a write to an attribute of something
    /// that already exists.
    Set { end: usize },
    /// `include "x" ignore missing with context`: introduces nothing, but its trailing words
    /// are keywords and were painting as variables.
    Include,
    /// `block name scoped required`: the name is a [`TokenType::Label`] being defined and the
    /// two modifiers are keywords. `endblock name` — Jinja lets the close repeat it — is the
    /// same label, referenced.
    Block { defines: bool },
    /// `filter upper`: the word after the tag name is a filter, the same thing as after `|`.
    Filter,
    Other,
}

/// Words that are keywords only in the statement that owns them. `{% if context %}` reads a
/// variable called `context`; `{% import "x" as m with context %}` does not.
const IMPORT_WORDS: &[&str] = &["context", "ignore", "missing"];
const BLOCK_WORDS: &[&str] = &["scoped", "required"];

impl Shape {
    fn of(toks: &[lexer::Token], text: &str) -> Shape {
        let is = |t: &lexer::Token, w: &str| t.kind == lexer::Kind::Name && t.span.slice(text) == w;
        let find = |from: usize, w: &str| toks.iter().skip(from).position(|t| is(t, w)).map(|p| p + from);
        let Some(first) = toks.first().filter(|t| t.kind == lexer::Kind::Name) else {
            return Shape::Other;
        };
        match first.span.slice(text) {
            "for" => Shape::For { in_at: find(1, "in") },
            "macro" => {
                let open = toks.iter().position(|t| t.kind == lexer::Kind::Lparen);
                let close = toks.iter().rposition(|t| t.kind == lexer::Kind::Rparen);
                Shape::Macro { parens: open.zip(close).filter(|(o, c)| o < c) }
            }
            "import" => Shape::Import,
            "from" => {
                let names = find(1, "import").map(|start| {
                    let end = find(start + 1, "with")
                        .or_else(|| find(start + 1, "without"))
                        .unwrap_or(toks.len());
                    (start + 1, end)
                });
                Shape::From { names }
            }
            "set" => {
                let end = toks
                    .iter()
                    .position(|t| matches!(t.kind, lexer::Kind::Assign | lexer::Kind::Pipe))
                    .unwrap_or(toks.len());
                Shape::Set { end }
            }
            "include" | "extends" => Shape::Include,
            "block" => Shape::Block { defines: true },
            "endblock" => Shape::Block { defines: false },
            "filter" => Shape::Filter,
            _ => Shape::Other,
        }
    }

    /// The type and modifier for the name at `i`, if this statement introduces it or paints
    /// it because of what it introduces. `None` hands the name to the generic arms.
    fn introduces(self, toks: &[lexer::Token], i: usize, text: &str) -> Option<(TokenType, bool)> {
        let word = |j: usize| toks.get(j).filter(|t| t.kind == lexer::Kind::Name).map(|t| t.span.slice(text));
        let prev_kind = i.checked_sub(1).map(|j| toks[j].kind);
        match self {
            Shape::For { in_at } if i > 0 && in_at.is_some_and(|at| i < at) => {
                Some((TokenType::Variable, true))
            }
            // Keywords first, so `import` in `from … import` never reaches the name range
            // below and `context` never reaches the fallback.
            Shape::Import | Shape::From { .. } | Shape::Include
                if word(i).is_some_and(|w| IMPORT_WORDS.contains(&w)) =>
            {
                Some((TokenType::Keyword, false))
            }
            Shape::From { .. } if word(i) == Some("import") => Some((TokenType::Keyword, false)),
            Shape::Block { .. } if word(i).is_some_and(|w| BLOCK_WORDS.contains(&w)) => {
                Some((TokenType::Keyword, false))
            }
            Shape::Block { defines } if i == 1 => Some((TokenType::Label, defines)),
            Shape::Filter if i == 1 => Some((TokenType::Function, false)),
            Shape::Macro { .. } if i == 1 => Some((TokenType::Function, true)),
            Shape::Macro { parens: Some((open, close)) }
                if open < i
                    && i < close
                    && matches!(prev_kind, Some(lexer::Kind::Lparen | lexer::Kind::Comma)) =>
            {
                Some((TokenType::Parameter, true))
            }
            Shape::Import if word(i - 1) == Some("as") => Some((TokenType::Namespace, true)),
            Shape::From { names: Some((start, end)) }
                if start <= i && i < end && word(i) != Some("as") =>
            {
                // `b as c` lands as `c`; `b` is the other file's name for it. The `as`
                // itself falls through to `WORD_OPERATORS`.
                let renamed_away = word(i + 1) == Some("as");
                Some((TokenType::Function, !renamed_away))
            }
            Shape::Set { end }
                if i > 0
                    && i < end
                    && prev_kind != Some(lexer::Kind::Dot)
                    && toks.get(i + 1).map(|t| t.kind) != Some(lexer::Kind::Dot) =>
            {
                Some((TokenType::Variable, true))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(text, type)` for every token, which is what a colour actually is.
    fn toks(src: &str) -> Vec<(&str, TokenType)> {
        tokens(src, &Delimiters::default(), true)
            .into_iter()
            .map(|t| (t.span.slice(src), t.ty))
            .collect()
    }

    /// Non-ASCII anywhere in a template must not crash the request.
    ///
    /// `find_end` walks one **byte** at a time and used to slice `self.src[i..]` to test for
    /// the closing delimiter, which panics the moment `i` lands inside a multi-byte
    /// character. `{{ café_port }}` was a hard crash — on the `.j2` path as much as in a
    /// YAML scalar, so this shipped broken for any name that is not ASCII.
    ///
    /// A sweep rather than one case, because the panic depends on where the character sits
    /// relative to the bracket walker: inside a quoted string it never fired (`skip_string`
    /// jumps the literal wholesale) and in a comment it never fired (`find`, not the
    /// walker), so a single well-chosen example proves much less than it looks.
    /// **42 of these 72 panic without the fix.**
    #[test]
    fn non_ascii_anywhere_in_a_template_does_not_panic() {
        let pieces = ["café", "שלום", "🎉", "日本語", "ß", "e\u{301}"];
        let shapes: Vec<String> = pieces
            .iter()
            .flat_map(|w| {
                vec![
                    format!("{{{{ {w} }}}}"),
                    format!("{{{{ {w}|upper }}}}"),
                    format!("{{{{ a.{w} }}}}"),
                    format!("{{{{ f({w}) }}}}"),
                    format!("{{% if {w} %}}x{{% endif %}}"),
                    format!("{{% raw %}}{w} {{% endraw %}}"),
                    format!("{{% for {w} in xs %}}{w}{{% endfor %}}"),
                    format!("{w}{{{{ a }}}}{w}"),
                    format!("{{{{ '{w}' ~ b }}}}"),
                    // Unterminated: the walker runs to the end of the string, which is the
                    // path that dereferences the last partial character.
                    format!("{{{{ {w}"),
                    format!("{{# {w} #}}"),
                    format!("#jinja2: block_start_string:'<%'\n<% if {w} %>y<% endif %>"),
                ]
            })
            .collect();
        let panics: Vec<&String> = shapes
            .iter()
            .filter(|src| {
                let s = (*src).clone();
                std::panic::catch_unwind(move || tokens(&s, &Delimiters::default(), true)).is_err()
            })
            .collect();
        assert!(panics.is_empty(), "{panics:#?}");
        // The control: the sweep must actually be reaching the tokenizer, or an empty
        // `panics` means nothing. Every ASCII-delimited shape here produces tokens.
        assert!(
            tokens("{{ café_port }}", &Delimiters::default(), true).len() == 3,
            "the sweep's subject must tokenize, not merely survive"
        );
    }

    /// The literal `{# … #}` on a comment-renamed template is repainted, so the grammar's
    /// guess is recoverable.
    ///
    /// Verified against ansible-core 2.21.3 rather than assumed: rendering
    /// `demo/templates/moved_comments.conf.j2` writes `keepalive {# not a comment #} 65;`
    /// into the destination verbatim, so this range really is output and the grammar's
    /// `comment.block.jinja` on it really is wrong.
    #[test]
    fn a_literal_curly_comment_is_repainted_when_the_delimiters_moved() {
        let src = "#jinja2: comment_start_string:'<#', comment_end_string:'#>'\n\
                   <# real #>\nkeepalive {# not a comment #} 65;\n";
        let got = toks(src);
        let repainted: Vec<_> =
            got.iter().filter(|(_, ty)| *ty == TokenType::Text).map(|(t, _)| *t).collect();
        assert_eq!(repainted, ["{# not a comment #}"], "{got:?}");
        // And the *real* comment is still a comment, so the repaint did not swallow it.
        let comments: Vec<_> =
            got.iter().filter(|(_, ty)| *ty == TokenType::Comment).map(|(t, _)| *t).collect();
        assert_eq!(comments, ["<# real #>"], "{got:?}");
    }

    /// The demo fixture itself, not a paraphrase of it.
    ///
    /// Rule 4: the file's `NO HINT` label is a claim about what we do, and a hand-written
    /// label rots. It also uses double quotes in its header where the test above uses single
    /// ones, so this is the only thing checking the bytes that actually ship.
    ///
    /// The claim was checked against ansible-core 2.21.3 first — rendering this template
    /// writes `keepalive {# not a comment #} 65;` to the destination verbatim.
    #[test]
    fn the_moved_comments_demo_paints_its_literal_curly_run_as_text() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../demo/templates/moved_comments.conf.j2");
        let src = std::fs::read_to_string(&path).expect("demo fixture is missing");
        let got = tokens(&src, &Delimiters::default(), true);
        let repainted: Vec<&str> = got
            .iter()
            .filter(|t| t.ty == TokenType::Text)
            .map(|t| t.span.slice(&src))
            .collect();
        assert_eq!(repainted, ["{# not a comment #}"], "{got:?}");
        // The angle form is the real comment here, so it must still be one — otherwise the
        // repaint could be passing by painting everything.
        assert!(
            got.iter().any(|t| t.ty == TokenType::Comment
                && t.span.slice(&src).starts_with("<# GOOD")),
            "the real comments must survive: {got:?}"
        );
    }

    /// The control, and the reason the test above means anything: with the delimiters left
    /// alone the grammar is right, so there is nothing to repaint and we emit no `Text`.
    /// Without this a `Text` token on every template would pass the assertion above.
    #[test]
    fn without_the_header_the_same_text_is_a_real_comment_and_is_not_repainted() {
        let src = "keepalive {# not a comment #} 65;\n";
        let got = toks(src);
        assert!(
            got.iter().all(|(_, ty)| *ty != TokenType::Text),
            "nothing to repaint when the grammar is right: {got:?}"
        );
        let comments: Vec<_> =
            got.iter().filter(|(_, ty)| *ty == TokenType::Comment).map(|(t, _)| *t).collect();
        assert_eq!(comments, ["{# not a comment #}"], "{got:?}");
    }

    /// The delimiters themselves get a token.
    ///
    /// A regression, and the shape of the miss is worth keeping: when the client's tag rules
    /// were removed this function was already being handed both spans — `span` covers the
    /// delimiters, `inner` only the content — and used only `inner`, so `{%` and `%}` stopped
    /// being painted by anything. Every test written at the time asserted what was *inside* a
    /// tag, so nothing caught it.
    #[test]
    fn the_delimiters_are_tokens_too_not_just_what_is_between_them() {
        let got = toks("{% if x %}{{ y }}");
        let delims: Vec<_> =
            got.iter().filter(|(_, ty)| *ty == TokenType::Delimiter).map(|(t, _)| *t).collect();
        assert_eq!(delims, ["{%", "%}", "{{", "}}"], "{got:?}");
        // Still no zero-width tokens, which a client may treat as malformed.
        assert!(
            tokens("{%%}", &Delimiters::default(), true).iter().all(|t| t.span.end > t.span.start)
        );
    }

    /// And they follow the file's real delimiters, which is the half a grammar cannot do:
    /// under this header the tokens belong to `<%`/`%>`, and the literal `{%` gets none.
    #[test]
    fn the_delimiter_tokens_follow_an_overridden_pair() {
        let src = "#jinja2: block_start_string:'<%', block_end_string:'%>'\n<% if x %>\nand {% no %}\n";
        let got = toks(src);
        let delims: Vec<_> =
            got.iter().filter(|(_, ty)| *ty == TokenType::Delimiter).map(|(t, _)| *t).collect();
        assert!(delims.contains(&"<%"), "{delims:?}");
        assert!(delims.contains(&"%>"), "{delims:?}");
        assert!(!delims.contains(&"{%"), "painted a literal brace as a delimiter: {delims:?}");
    }

    /// The case the client's TextMate grammar gets exactly backwards, and the reason this
    /// module exists. A grammar hardcodes `{%`, so on a template whose header renames the
    /// delimiters it paints **nothing** on the real tags and paints a literal `{%` as a tag.
    /// Measured with `vscode-textmate` before this was written, on this same source.
    #[test]
    fn an_overridden_delimiter_moves_the_tokens_to_the_real_tags() {
        let src = "#jinja2: block_start_string:'<%', block_end_string:'%>'\n\
                   <% if x %>t<% endif %>\n\
                   and {% not a tag %} here\n";
        let got = toks(src);
        // The real tags are read.
        assert!(got.contains(&("if", TokenType::Keyword)), "{got:?}");
        assert!(got.contains(&("endif", TokenType::Keyword)), "{got:?}");
        assert!(got.contains(&("x", TokenType::Variable)), "{got:?}");
        // And the literal `{%` is data, so nothing inside it is a token at all.
        for dead in ["not", "a", "tag"] {
            assert!(!got.iter().any(|(t, _)| *t == dead), "{dead:?} painted: {got:?}");
        }
    }

    /// The control for that one: with no header, the same two lines swap roles. Without it
    /// the assertion above would also pass on a reader that simply painted less.
    #[test]
    fn without_the_header_the_default_delimiters_win() {
        let src = "<% if x %>t<% endif %>\nand {% not a tag %} here\n";
        let got = toks(src);
        assert!(got.contains(&("not", TokenType::Keyword)), "{got:?}");
        assert!(got.contains(&("tag", TokenType::Variable)), "{got:?}");
        assert!(!got.iter().any(|(t, _)| *t == "endif"), "{got:?}");
    }

    /// A raw body is data — the T-216 shape, now as colour.
    #[test]
    fn nothing_inside_a_raw_body_is_a_token() {
        assert_eq!(toks(r#"{% raw %}printf "x{%s}"{% endraw %}"#), []);
        // Control: the same text outside a raw body does produce tokens.
        assert!(!toks(r#"{{ printf }}"#).is_empty());
    }

    #[test]
    fn a_tag_name_is_a_keyword_but_the_same_word_in_an_expression_is_not() {
        assert!(toks("{% include 'a.j2' %}").contains(&("include", TokenType::Keyword)));
        assert!(toks("{{ include }}").contains(&("include", TokenType::Variable)));
    }

    #[test]
    fn a_filter_and_a_call_are_functions_and_a_bare_name_is_a_variable() {
        let got = toks("{{ app_port | default(8080) }}");
        assert!(got.contains(&("app_port", TokenType::Variable)), "{got:?}");
        assert!(got.contains(&("default", TokenType::Function)), "{got:?}");
        assert!(got.contains(&("8080", TokenType::Number)), "{got:?}");
        assert!(got.contains(&("|", TokenType::Operator)), "{got:?}");
    }

    /// `ansible_facts.hostname`: the root is a variable and the name after the dot is not.
    /// The call is the control — `upstream` in `m.upstream(` sits after a dot too and must
    /// stay a function, otherwise this passes by painting everything after a dot.
    #[test]
    fn a_name_after_a_dot_is_a_property_unless_it_is_called() {
        let got = toks("{{ ansible_facts.hostname | default(x.y) }}");
        assert!(got.contains(&("ansible_facts", TokenType::Variable)), "{got:?}");
        assert!(got.contains(&("hostname", TokenType::Property)), "{got:?}");
        assert!(got.contains(&("x", TokenType::Variable)), "{got:?}");
        assert!(got.contains(&("y", TokenType::Property)), "{got:?}");
        let call = toks("{{ m.upstream('web') }}");
        assert!(call.contains(&("m", TokenType::Variable)), "{call:?}");
        assert!(call.contains(&("upstream", TokenType::Function)), "{call:?}");
    }

    /// `(text, declaration)` for every variable token — the modifier is the whole question.
    fn decls(src: &str) -> Vec<(&str, bool)> {
        tokens(src, &Delimiters::default(), true)
            .into_iter()
            .filter(|t| t.ty == TokenType::Variable)
            .map(|t| (t.span.slice(src), t.declaration))
            .collect()
    }

    /// `{% for h in hosts %}` introduces `h` and reads `hosts`. Tuple targets are all
    /// introduced; everything after the first `in` — the iterable, an `if` filter with its
    /// own `in` — is a read, and so is the same name in the body.
    #[test]
    fn the_names_between_for_and_in_are_declarations_and_nothing_after_in_is() {
        assert_eq!(decls("{% for h in hosts %}"), [("h", true), ("hosts", false)]);
        assert_eq!(
            decls("{% for k, v in d.items() if k in wanted %}"),
            [("k", true), ("v", true), ("d", false), ("k", false), ("wanted", false)]
        );
        assert_eq!(decls("{{ h }}"), [("h", false)]);
        // Controls: `in` as a membership test is not a loop, and a `for` that is not the
        // tag name is an ordinary variable, so neither may declare anything.
        assert_eq!(decls("{% if h in hosts %}"), [("h", false), ("hosts", false)]);
        assert_eq!(decls("{{ for }}"), [("for", false)]);
    }

    /// `(text, type, declaration)` for every name — the full answer for a statement that
    /// introduces something.
    fn names(src: &str) -> Vec<(&str, TokenType, bool)> {
        tokens(src, &Delimiters::default(), true)
            .into_iter()
            .filter(|t| {
                !matches!(
                    t.ty,
                    TokenType::Delimiter | TokenType::Operator | TokenType::String | TokenType::Number
                )
            })
            .map(|t| (t.span.slice(src), t.ty, t.declaration))
            .collect()
    }

    /// `{% macro %}` defines a function and its parameters; a default value is a read.
    /// The control is a *call* of the same name, which must stay an undeclared function.
    #[test]
    fn a_macro_declares_its_name_as_a_function_and_its_arguments_as_parameters() {
        use TokenType::*;
        assert_eq!(
            names("{% macro upstream(group, sep=default_sep) %}"),
            [
                ("macro", Keyword, false),
                ("upstream", Function, true),
                ("group", Parameter, true),
                ("sep", Parameter, true),
                ("default_sep", Variable, false),
            ]
        );
        assert_eq!(names("{% macro bare() %}"), [("macro", Keyword, false), ("bare", Function, true)]);
        assert_eq!(
            names("{% call upstream('web') %}"),
            [("call", Keyword, false), ("upstream", Function, false)]
        );
    }

    /// `import … as m` introduces a namespace; `from … import a, b as c` introduces `a` and
    /// `c`, and `b` is the other file's name for `c`. `with context` is not a macro.
    #[test]
    fn import_and_from_import_declare_what_lands_in_scope() {
        use TokenType::*;
        assert_eq!(
            names("{% import 'macros.j2' as m %}"),
            [("import", Keyword, false), ("as", WordOperator, false), ("m", Namespace, true)]
        );
        assert_eq!(
            names("{% from 'macros.j2' import a, b as c with context %}"),
            [
                ("from", Keyword, false),
                ("import", Keyword, false),
                ("a", Function, true),
                ("b", Function, false),
                ("as", WordOperator, false),
                ("c", Function, true),
                ("with", WordOperator, false),
                ("context", Keyword, false),
            ]
        );
        // Control: the same names in an expression are plain reads.
        assert_eq!(names("{{ m }}"), [("m", Variable, false)]);
    }

    /// The words that are keywords only inside the statement that owns them. The controls
    /// are the same spellings outside it: `{% if context %}` reads a variable, and so does
    /// `{{ missing }}` — a user is allowed to name a variable `scoped`.
    #[test]
    fn statement_words_are_keywords_in_their_statement_and_variables_elsewhere() {
        use TokenType::*;
        assert_eq!(
            names("{% include 'a.j2' ignore missing with context %}"),
            [
                ("include", Keyword, false),
                ("ignore", Keyword, false),
                ("missing", Keyword, false),
                ("with", WordOperator, false),
                ("context", Keyword, false),
            ]
        );
        assert_eq!(
            names("{% import 'a.j2' as m without context %}"),
            [
                ("import", Keyword, false),
                ("as", WordOperator, false),
                ("m", Namespace, true),
                ("without", WordOperator, false),
                ("context", Keyword, false),
            ]
        );
        assert_eq!(
            names("{% block body scoped required %}"),
            [("block", Keyword, false), ("body", Label, true), ("scoped", Keyword, false), ("required", Keyword, false)]
        );
        // The close may repeat the name: the same label, referenced rather than defined. And
        // a bare `body` elsewhere is a variable — the label leaves nothing in the memory map.
        assert_eq!(names("{% endblock body %}"), [("endblock", Keyword, false), ("body", Label, false)]);
        assert_eq!(names("{% block body %}{{ body }}"), [("block", Keyword, false), ("body", Label, true), ("body", Variable, false)]);
        assert_eq!(names("{% filter upper %}"), [("filter", Keyword, false), ("upper", Function, false)]);
        // Controls.
        assert_eq!(names("{% if context %}"), [("if", Keyword, false), ("context", Variable, false)]);
        assert_eq!(names("{{ missing }}"), [("missing", Variable, false)]);
        assert_eq!(names("{% set scoped = 1 %}"), [("set", Keyword, false), ("scoped", Variable, true)]);
        assert_eq!(names("{% include tuning_file %}"), [("include", Keyword, false), ("tuning_file", Variable, false)]);
    }

    /// The demo carries three of these: `with context` on the chain root's import, `scoped`
    /// on its body block, and `ignore missing` on `optional.conf.j2`'s include.
    #[test]
    fn the_demo_paints_its_statement_words_as_keywords() {
        let keywords = |name: &str| -> Vec<String> {
            let src = demo(name);
            tokens(&src, &Delimiters::default(), true)
                .iter()
                .filter(|t| t.ty == TokenType::Keyword)
                .map(|t| t.span.slice(&src).to_string())
                .filter(|w| IMPORT_WORDS.contains(&w.as_str()) || BLOCK_WORDS.contains(&w.as_str()))
                .collect()
        };
        assert_eq!(keywords("app.conf.j2"), ["context", "scoped"]);
        assert_eq!(keywords("optional.conf.j2"), ["ignore", "missing"]);
    }

    /// `set` introduces what is left of `=` or `|`; an attribute write introduces nothing.
    #[test]
    fn set_declares_its_targets_but_not_an_attribute_it_writes_to() {
        use TokenType::*;
        assert_eq!(
            names("{% set a, b = c %}"),
            [("set", Keyword, false), ("a", Variable, true), ("b", Variable, true), ("c", Variable, false)]
        );
        assert_eq!(
            names("{% set x | upper %}"),
            [("set", Keyword, false), ("x", Variable, true), ("upper", Function, false)]
        );
        assert_eq!(
            names("{% set ns.attr = 1 %}"),
            [("set", Keyword, false), ("ns", Variable, false), ("attr", Property, false)]
        );
        assert_eq!(names("{% set block_form %}"), [("set", Keyword, false), ("block_form", Variable, true)]);
    }

    /// A bare use after the statement that introduced the name paints as what it introduced.
    /// Three controls, each a way this could be wrong: the use *before* the declaration is a
    /// variable (source order, which is also Jinja's scope rule); a later `for` over the same
    /// name takes it back; and a parameter's body use stays a variable, because a parameter
    /// is scoped to its macro and this memory does not model that.
    #[test]
    fn a_later_use_of_an_introduced_name_paints_as_what_introduced_it() {
        use TokenType::*;
        assert_eq!(
            names("{% import 'x' as m %}{{ m.a }}{{ m }}"),
            [
                ("import", Keyword, false),
                ("as", WordOperator, false),
                ("m", Namespace, true),
                ("m", Namespace, false),
                ("a", Property, false),
                ("m", Namespace, false),
            ]
        );
        assert_eq!(
            names("{% from 'x' import f %}{{ f }}"),
            [("from", Keyword, false), ("import", Keyword, false), ("f", Function, true), ("f", Function, false)]
        );
        assert_eq!(
            names("{{ m }}{% import 'x' as m %}"),
            [("m", Variable, false), ("import", Keyword, false), ("as", WordOperator, false), ("m", Namespace, true)]
        );
        assert_eq!(
            names("{% import 'x' as m %}{% for m in xs %}{{ m }}"),
            [
                ("import", Keyword, false),
                ("as", WordOperator, false),
                ("m", Namespace, true),
                ("for", Keyword, false),
                ("m", Variable, true),
                ("in", WordOperator, false),
                ("xs", Variable, false),
                ("m", Variable, false),
            ]
        );
        assert_eq!(
            names("{% macro f(p) %}{{ p }}{% endmacro %}"),
            [
                ("macro", Keyword, false),
                ("f", Function, true),
                ("p", Parameter, true),
                ("p", Variable, false),
                ("endmacro", Keyword, false),
            ]
        );
    }

    /// The demo's `{{ m.upstream('web') }}`, with the `{% import "macros.j2" as m %}` above
    /// it: every `m` in the file is the namespace, and exactly the first is its declaration.
    #[test]
    fn the_chain_root_demo_paints_the_imported_namespace_at_its_use() {
        use TokenType::*;
        let src = demo("app.conf.j2");
        let ms: Vec<(TokenType, bool)> = tokens(&src, &Delimiters::default(), true)
            .iter()
            .filter(|t| t.span.slice(&src) == "m")
            .map(|t| (t.ty, t.declaration))
            .collect();
        assert_eq!(ms, [(Namespace, true), (Namespace, false)]);
    }

    /// A keyword argument at a call names a parameter and declares nothing. Controls: `=`
    /// after `set` is a declaration, `==` is not `=`, and a positional argument is a read.
    #[test]
    fn a_keyword_argument_is_a_parameter_that_declares_nothing() {
        use TokenType::*;
        assert_eq!(
            names("{{ x | to_nice_yaml(indent=2, width=w) }}"),
            [
                ("x", Variable, false),
                ("to_nice_yaml", Function, false),
                ("indent", Parameter, false),
                ("width", Parameter, false),
                ("w", Variable, false),
            ]
        );
        assert_eq!(names("{{ dict(a=1) }}"), [("dict", Function, false), ("a", Parameter, false)]);
        assert_eq!(names("{% set a = 1 %}"), [("set", Keyword, false), ("a", Variable, true)]);
        assert_eq!(names("{{ f(a == b) }}"), [("f", Function, false), ("a", Variable, false), ("b", Variable, false)]);
        // And it leaves no memory behind: a later bare `indent` is still a variable.
        assert_eq!(
            names("{{ f(indent=2) }}{{ indent }}"),
            [("f", Function, false), ("indent", Parameter, false), ("indent", Variable, false)]
        );
    }

    /// The demo's `comment(decoration='# ')` is the call side; `macros.j2` has the
    /// definition side. Between the two files, every parameter token and its modifier.
    #[test]
    fn the_demo_has_one_keyword_argument_and_one_macro_parameter() {
        let params = |name: &str| -> Vec<(String, bool)> {
            let src = demo(name);
            tokens(&src, &Delimiters::default(), true)
                .iter()
                .filter(|t| t.ty == TokenType::Parameter)
                .map(|t| (t.span.slice(&src).to_string(), t.declaration))
                .collect()
        };
        assert_eq!(params("app.conf.j2"), [("decoration".to_string(), false)]);
        assert_eq!(params("macros.j2"), [("group".to_string(), true), ("port".to_string(), true)]);
    }

    /// The two halves of the demo's inheritance: `base.conf.j2` defines both slots, and
    /// `app.conf.j2` fills one. Every block name is a label, and every one is a definition —
    /// a child's `{% block body %}` defines its own version, it does not reference the parent's.
    #[test]
    fn the_demo_block_names_are_labels() {
        let labels = |name: &str| -> Vec<(String, bool)> {
            let src = demo(name);
            tokens(&src, &Delimiters::default(), true)
                .iter()
                .filter(|t| t.ty == TokenType::Label)
                .map(|t| (t.span.slice(&src).to_string(), t.declaration))
                .collect()
        };
        assert_eq!(labels("base.conf.j2"), [("header".to_string(), true), ("body".to_string(), true)]);
        assert_eq!(labels("app.conf.j2"), [("header".to_string(), true), ("body".to_string(), true)]);
    }

    fn demo(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo/templates").join(name);
        std::fs::read_to_string(&path).expect("demo fixture is missing")
    }

    /// The demo fixture's declarations, as an exact ordered set: the `GOOD` label in
    /// `app.conf.j2` names four, and an extra or missing one is a wrong label.
    #[test]
    fn the_chain_root_demo_declares_exactly_these_names() {
        use TokenType::*;
        let src = demo("app.conf.j2");
        let declared: Vec<(&str, TokenType)> = tokens(&src, &Delimiters::default(), true)
            .iter()
            .filter(|t| t.declaration)
            .map(|t| (t.span.slice(&src), t.ty))
            .collect();
        assert_eq!(
            declared,
            [
                ("m", Namespace),
                ("listen_line", Function),
                ("header", Label),
                ("body", Label),
                ("listen_port", Variable),
                ("h", Variable),
            ]
        );
    }

    /// The other half of that label lives in `macros.j2`: the macro's own name and parameter.
    /// The body's `{{ group }}` is the control — the same name, read, undeclared.
    #[test]
    fn the_macros_demo_declares_the_macro_and_its_parameter_only() {
        use TokenType::*;
        let src = demo("macros.j2");
        let got: Vec<(&str, TokenType, bool)> = tokens(&src, &Delimiters::default(), true)
            .iter()
            .filter(|t| t.span.slice(&src) == "upstream" || t.span.slice(&src) == "group")
            .map(|t| (t.span.slice(&src), t.ty, t.declaration))
            .collect();
        assert_eq!(
            got,
            [("upstream", Function, true), ("group", Parameter, true), ("group", Variable, false)]
        );
    }

    /// The demo fixture itself, for the same reason as the moved-comments one below: its
    /// `GOOD` label claims a property token, and a hand-written label rots.
    #[test]
    fn the_chain_root_demo_paints_the_dotted_name_as_a_property() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../demo/templates/app.conf.j2");
        let src = std::fs::read_to_string(&path).expect("demo fixture is missing");
        let got = tokens(&src, &Delimiters::default(), true);
        let props: Vec<&str> = got
            .iter()
            .filter(|t| t.ty == TokenType::Property)
            .map(|t| t.span.slice(&src))
            .collect();
        assert_eq!(props, ["hostname"], "{got:?}");
        // `m.upstream(` is the control: a name after a dot that is called is still a call.
        assert!(
            got.iter().any(|t| t.ty == TokenType::Function && t.span.slice(&src) == "upstream"),
            "{got:?}"
        );
    }

    #[test]
    fn comments_and_strings_classify() {
        assert_eq!(toks("{# hi #}"), [("{# hi #}", TokenType::Comment)]);
        assert!(toks("{{ 'x' }}").contains(&("'x'", TokenType::String)));
    }

    /// A tag name, a word operator and a literal are three different things, and were one.
    ///
    /// The precedent, measured with `vscode-textmate` over VS Code's own Go and Python
    /// grammars and resolved through Dark Modern's chain: `if`/`return` are
    /// `keyword.control` `#C586C0`, `and` is `keyword.operator.logical` `#569cd6`, `true`
    /// and `True` are `constant.language` `#569cd6`, and Go's `&&` is left at the default
    /// foreground. Collapsing all of those onto `keyword` painted `{% if x and true %}` in
    /// one colour, which neither language does.
    #[test]
    fn a_tag_name_a_word_operator_and_a_literal_are_three_kinds() {
        let got = toks("{% if a and true %}");
        assert!(got.contains(&("if", TokenType::Keyword)), "{got:?}");
        assert!(got.contains(&("and", TokenType::WordOperator)), "{got:?}");
        assert!(got.contains(&("true", TokenType::Constant)), "{got:?}");
        assert!(got.contains(&("a", TokenType::Variable)), "{got:?}");
        // A symbolic operator stays distinct from a word one.
        assert!(toks("{{ x | y }}").contains(&("|", TokenType::Operator)));
    }

    /// A test after `is` is function-shaped, like a filter after `|`.
    #[test]
    fn the_name_after_is_is_a_test_not_a_variable() {
        assert!(toks("{{ x is defined }}").contains(&("defined", TokenType::Function)));
        assert!(toks("{{ x is not defined }}").contains(&("defined", TokenType::Function)));
        assert!(toks("{{ x is sameas y }}").contains(&("sameas", TokenType::Function)));
        // The subject and the far operand are still variables, so this did not become
        // "anything near an `is` is a function".
        let got = toks("{{ x is sameas y }}");
        assert!(got.contains(&("x", TokenType::Variable)), "{got:?}");
        assert!(got.contains(&("y", TokenType::Variable)), "{got:?}");
        // And a literal after `is` stays a literal rather than flipping kind by position.
        assert!(toks("{{ x is none }}").contains(&("none", TokenType::Constant)));
    }

    /// All six spellings, and the three lookalikes that are really variables — the same split
    /// jinja2's parser makes, and the control that [[T-220]] turned on.
    #[test]
    fn every_literal_spelling_is_a_constant_and_the_lookalikes_are_not() {
        for lit in ["true", "True", "false", "False", "none", "None"] {
            let src = format!("{{{{ {lit} }}}}");
            let got = tokens(&src, &Delimiters::default(), true);
            assert!(
                got.iter().any(|t| t.ty == TokenType::Constant && t.span.slice(&src) == lit),
                "{lit} should be a constant: {got:?}"
            );
        }
        for name in ["null", "nil", "TRUE"] {
            let src = format!("{{{{ {name} }}}}");
            let got = tokens(&src, &Delimiters::default(), true);
            assert!(
                got.iter().any(|t| t.ty == TokenType::Variable && t.span.slice(&src) == name),
                "{name} is a name jinja2 looks up, not a literal: {got:?}"
            );
        }
    }

    /// A header can turn line statements on, and then a `#` line is a statement rather than
    /// text — another shape no delimiter-matching regex reaches.
    #[test]
    fn a_line_statement_from_a_header_is_read_as_a_statement() {
        let got = toks("#jinja2: line_statement_prefix:'#'\n# if x\nbody\n# endif\n");
        assert!(got.contains(&("if", TokenType::Keyword)), "{got:?}");
        assert!(got.contains(&("endif", TokenType::Keyword)), "{got:?}");
    }

    /// What a corpus of real templates hands to the generic arms. Not an assertion — a
    /// survey, so the shapes T-217 handles are the ones templates contain rather than the
    /// ones we thought of. Prints three tables: every statement tag with whether a `Shape`
    /// reads it, the bare names painted `variable` most often (a builtin like `loop` shows up
    /// here), and sample statements for each unshaped tag.
    ///
    /// `ANSIBLE_CORPUS` is one tree or a directory of trees, as for `when_coverage`.
    #[test]
    #[ignore = "corpus survey: ANSIBLE_CORPUS=<path> cargo test -p ansible-core --lib highlight_survey -- --ignored --nocapture"]
    fn highlight_survey() {
        use std::collections::BTreeMap;
        let Ok(root) = std::env::var("ANSIBLE_CORPUS") else { return };
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "j2") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        walk(std::path::Path::new(&root), &mut files);

        // Tags that close or branch: a `Shape` has nothing to read after them.
        const STRUCTURAL: &[&str] = &[
            "if", "elif", "else", "endif", "endfor", "endmacro", "endblock", "endset", "raw",
            "endraw", "endcall", "endfilter", "endwith", "break", "continue", "extends",
        ];
        let d = Delimiters::default();
        let (mut parsed, mut unparsed) = (0, 0);
        let mut tags: BTreeMap<String, (usize, bool)> = BTreeMap::new();
        let mut bare: BTreeMap<String, usize> = BTreeMap::new();
        let mut samples: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for f in &files {
            let Ok(src) = std::fs::read_to_string(f) else { continue };
            let Ok((blocks, _)) = template::document_in(&src, &d, true) else {
                unparsed += 1;
                continue;
            };
            parsed += 1;
            for b in blocks.iter().filter(|b| b.kind == template::Kind::Statement) {
                let text = b.inner.slice(&src);
                let Ok(ts) = lexer::tokens(text) else { continue };
                let Some(first) = ts.first().filter(|t| t.kind == lexer::Kind::Name) else { continue };
                let tag = first.span.slice(text).to_string();
                let shaped = Shape::of(&ts, text) != Shape::Other;
                let e = tags.entry(tag.clone()).or_insert((0, shaped));
                e.0 += 1;
                if !shaped && !STRUCTURAL.contains(&tag.as_str()) {
                    let s = samples.entry(tag).or_default();
                    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if s.len() < 4 && !s.contains(&line) {
                        s.push(line);
                    }
                }
            }
            for t in tokens(&src, &d, true) {
                if t.ty == TokenType::Variable && !t.declaration {
                    *bare.entry(t.span.slice(&src).to_string()).or_default() += 1;
                }
            }
        }

        println!("\n{} templates, {parsed} parsed, {unparsed} refused\n", files.len());
        println!("{:<12} {:>6}  shaped", "tag", "n");
        let mut by_n: Vec<_> = tags.iter().collect();
        by_n.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
        for (tag, (n, shaped)) in by_n {
            println!("{tag:<12} {n:>6}  {}", if *shaped { "yes" } else { "-" });
        }
        println!("\nbare names painted variable, top 40:");
        let mut by_n: Vec<_> = bare.iter().collect();
        by_n.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (name, n) in by_n.iter().take(40) {
            println!("{n:>6}  {name}");
        }
        println!("\nunshaped tags, sample statements:");
        for (tag, lines) in &samples {
            for l in lines {
                println!("  {tag:<10} {l}");
            }
        }
    }
}
