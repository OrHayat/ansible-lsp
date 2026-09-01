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
            template::Kind::Comment => out.push(SemToken { span: b.span, ty: TokenType::Comment }),
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
                inner_tokens(src, b.inner, b.kind == template::Kind::Statement, &mut out);
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
    inner_tokens(src, Span { start: 0, end: src.len() }, false, &mut out);
    out
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
        out.push(SemToken { span: Span { start, end }, ty: TokenType::Delimiter });
    }
}

/// Classify the contents of one `{% … %}` or `{{ … }}`.
///
/// `is_statement` marks the first name as the tag keyword — `include` in `{% include x %}`
/// is a keyword, while the identical text in `{{ include }}` is a variable.
fn inner_tokens(src: &str, inner: Span, is_statement: bool, out: &mut Vec<SemToken>) {
    let text = inner.slice(src);
    // An unlexable body is left to the grammar rather than half-painted.
    let Ok(toks) = lexer::tokens(text) else { return };
    let at = |s: Span| Span { start: inner.start + s.start, end: inner.start + s.end };

    for (i, t) in toks.iter().enumerate() {
        let ty = match t.kind {
            lexer::Kind::Eof => continue,
            lexer::Kind::Str => TokenType::String,
            lexer::Kind::Int | lexer::Kind::Float => TokenType::Number,
            lexer::Kind::Name => {
                let word = t.span.slice(text);
                let prev = i.checked_sub(1).map(|j| toks[j].kind);
                let next = toks.get(i + 1).map(|n| n.kind);
                if is_statement && i == 0 {
                    // The tag name. `include` here is a keyword; the identical text in
                    // `{{ include }}` is a variable.
                    TokenType::Keyword
                } else if LITERALS.contains(&word) {
                    TokenType::Constant
                } else if WORD_OPERATORS.contains(&word) {
                    TokenType::WordOperator
                } else if is_test_name(&toks, i, text) {
                    // `x is defined`, `x is not none`, `x is sameas y` — the name after `is`
                    // is a test, which is the same shape as a filter after `|` and was being
                    // called a variable.
                    TokenType::Function
                } else if prev == Some(lexer::Kind::Pipe) || next == Some(lexer::Kind::Lparen) {
                    // A filter after `|`, or anything being called. Both read as functions,
                    // and both are wrong to call a variable.
                    TokenType::Function
                } else {
                    TokenType::Variable
                }
            }
            _ => TokenType::Operator,
        };
        out.push(SemToken { span: at(t.span), ty });
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
}
