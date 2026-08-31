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
    Operator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemToken {
    pub span: Span,
    pub ty: TokenType,
}

/// Words the lexer returns as `Name` but which are keywords, not variables. `true`/`false`/
/// `none` are values rather than operators, and jinja2 accepts both capitalisations.
const KEYWORDS: &[&str] = &[
    "and", "or", "not", "in", "is", "if", "else", "elif", "as", "with", "without", "recursive",
    "true", "false", "none", "True", "False", "None",
];

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
    for b in blocks {
        match b.kind {
            template::Kind::Data => {}
            template::Kind::Comment => out.push(SemToken { span: b.span, ty: TokenType::Comment }),
            template::Kind::Statement => inner_tokens(src, b.inner, true, &mut out),
            template::Kind::Expression => inner_tokens(src, b.inner, false, &mut out),
        }
    }
    let _ = delims;
    out
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
                    TokenType::Keyword
                } else if KEYWORDS.contains(&word) {
                    TokenType::Keyword
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
    fn comments_strings_and_word_operators_classify() {
        assert_eq!(toks("{# hi #}"), [("{# hi #}", TokenType::Comment)]);
        assert!(toks("{{ 'x' }}").contains(&("'x'", TokenType::String)));
        assert!(toks("{% if a and b %}").contains(&("and", TokenType::Keyword)));
        assert!(toks("{{ true }}").contains(&("true", TokenType::Keyword)));
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
