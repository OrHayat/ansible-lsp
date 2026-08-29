//! The statement forms, and the block nesting they imply.
//!
//! [`super::template`] says where a `{% … %}` is; this says what it is. The two together are
//! what `env.from_string` does, and the reason both are needed is [`check`]'s job rather than
//! [`parse`]'s: **a reader that walks past tags it does not model cannot tell a tag it chose to
//! skip from a tag that does not exist**, so it can never say a template will not render.
//!
//! Measured on jinja2 3.1.6 — `env.parse` rejects every one of these and a four-tag skimmer
//! takes all nine silently:
//!
//! | template | jinja2 |
//! | --- | --- |
//! | `{% for x in xs %}{{ x }}` | unexpected end of template, looking for `endfor` |
//! | `{% if a %}x{% endfor %}` | unknown tag `endfor`, innermost block is `if` |
//! | `{% forr x in xs %}{% endforr %}` | unknown tag `forr` |
//! | `{% include 'a.j2' %}{% endif %}` | unknown tag `endif` |
//! | `{% macro m(a,) %}{% endmacro %}` | expected token `name`, got `)` |
//! | `{% set x = %}` | expected an expression |
//! | `{% for x in %}{% endfor %}` | expected an expression |
//! | `{% raw %}{% include 'x.j2' %}` | missing end of raw directive |
//! | `{{ x }` | unexpected `}` |
//!
//! Rows 6 and 7 are broken *inside tags a four-tag reader does not model*, so no amount of care
//! in an include reader reaches them (T-040).

use crate::parse::Span;

use super::ast::{Const, ExprKind};
use super::lexer::{self, Cause, Error, Kind, Token};
use super::parser;
use super::template::{self, Block, Delimiters};

/// The twelve `_statement_keywords`, plus `call` and `filter`, which `parse_statement`
/// dispatches separately. Anything else is `fail_unknown_tag`.
const OPENERS: &[(&str, &str)] = &[
    ("for", "endfor"),
    ("if", "endif"),
    ("block", "endblock"),
    ("macro", "endmacro"),
    ("call", "endcall"),
    ("filter", "endfilter"),
    ("with", "endwith"),
    ("autoescape", "endautoescape"),
    ("trans", "endtrans"),
    // `set` is both: `{% set x = 1 %}` is complete, `{% set x %}…{% endset %}` is a block.
    // Listed here so `endset` resolves to its opener; `parse` decides which form a given
    // `{% set %}` is before this table is consulted.
    ("set", "endset"),
];

/// Tags that take no body and so never open a block.
const STANDALONE: &[&str] = &["extends", "include", "from", "import", "print", "do", "set"];

/// Tags that continue an open block rather than opening one, and which openers accept them.
const CONTINUATIONS: &[(&str, &[&str])] =
    &[("else", &["if", "for", "trans"]), ("elif", &["if"]), ("pluralize", &["trans"])];

/// A template named by one of the four target-naming tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The literal target. Dynamic names are not references — upstream's
    /// `find_referenced_templates` yields `None` for them and so does this.
    pub template: String,
    /// The span of the literal, for go-to-definition.
    pub span: Span,
    pub tag: RefTag,
    /// `{% include ... ignore missing %}` — an absent target is legal by design and ansible
    /// renders nothing rather than failing, so it must never be reported as a miss. Only
    /// `include` takes the modifier; the other three tags leave this false.
    pub ignore_missing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefTag {
    Extends,
    Include,
    Import,
    From,
}

/// What one `{% … %}` is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    /// Opens a block; carries the end tag that must close it.
    Open { tag: String, end: &'static str, references: Vec<Reference> },
    /// Continues an open block (`else`, `elif`, `pluralize`).
    Continuation { tag: String },
    /// Closes a block.
    End { tag: String, opener: String },
    /// Complete in itself.
    Standalone { tag: String, references: Vec<Reference> },
}

/// A top-level `=`, which is what separates `{% set x = 1 %}` from `{% set x %}`. Depth
/// matters: the `=` in `{% set x = f(a=1) %}` is a keyword argument, not the assignment, and
/// a bare `{% set x %}` has neither.
fn assigns(body: &str, toks: &[Token], rest: usize) -> bool {
    let mut depth = 0i32;
    for t in &toks[rest..] {
        match t.kind {
            Kind::Lparen | Kind::Lbracket | Kind::Lbrace => depth += 1,
            Kind::Rparen | Kind::Rbracket | Kind::Rbrace => depth -= 1,
            Kind::Assign if depth == 0 => return true,
            _ => {}
        }
    }
    let _ = body;
    false
}

fn end_of(tag: &str) -> Option<&'static str> {
    OPENERS.iter().find(|(o, _)| *o == tag).map(|(_, e)| *e)
}

fn opener_of(end_tag: &str) -> Option<&'static str> {
    OPENERS.iter().find(|(_, e)| *e == end_tag).map(|(o, _)| *o)
}

fn continuation_openers(tag: &str) -> Option<&'static [&'static str]> {
    CONTINUATIONS.iter().find(|(c, _)| *c == tag).map(|(_, o)| *o)
}

/// A token's own text, for the bare-name modifiers that follow an expression.
fn slice(body: &str, span: Span) -> &str {
    body.get(span.start..span.end).unwrap_or("")
}

fn err(msg: impl Into<String>, span: Span) -> Error {
    Error { msg: msg.into(), span, cause: Cause::Parse }
}

/// Read one statement from a `{% … %}` body.
///
/// `inner` is the block's inner span, so every span this returns is absolute and can underline
/// the real document.
pub fn parse(src: &str, inner: Span) -> Result<Stmt, Error> {
    let body = inner.slice(src);
    let off = inner.start;
    let shift = |s: Span| Span { start: s.start + off, end: s.end + off };

    let toks = lexer::tokens(body).map_err(|e| Error { span: shift(e.span), ..e })?;
    let first = toks.first().copied().unwrap_or(Token { kind: Kind::Eof, span: Span { start: 0, end: 0 } });
    if first.kind != Kind::Name {
        return Err(err("tag name expected", shift(first.span)));
    }
    let tag = first.span.slice(body).to_string();
    let rest = 1usize;

    // Measured on the corpus: `{% set x %}…{% endset %}` is real and common enough that
    // treating `set` as always-standalone reported a live kubespray template as broken.
    if tag == "set" {
        return if assigns(body, &toks, rest) {
            let references = standalone_references(body, &toks, rest, &tag, &shift)?;
            Ok(Stmt::Standalone { tag, references })
        } else {
            Ok(Stmt::Open { tag, end: "endset", references: Vec::new() })
        };
    }

    if let Some(end) = end_of(&tag) {
        validate_opener(body, &toks, rest, &tag, shift)?;
        return Ok(Stmt::Open { tag, end, references: Vec::new() });
    }
    if let Some(opener) = opener_of(&tag) {
        return Ok(Stmt::End { tag, opener: opener.to_string() });
    }
    if continuation_openers(&tag).is_some() {
        return Ok(Stmt::Continuation { tag });
    }
    if STANDALONE.contains(&tag.as_str()) {
        let references = standalone_references(body, &toks, rest, &tag, &shift)?;
        return Ok(Stmt::Standalone { tag, references });
    }
    Err(err(format!("Encountered unknown tag '{tag}'."), shift(first.span)))
}

/// The parts of an opener this module models. `if`/`elif` and `for` must reach an expression,
/// because `{% for x in %}` and `{% set x = %}` are broken in a way only their own parser sees.
fn validate_opener(
    body: &str,
    toks: &[Token],
    rest: usize,
    tag: &str,
    shift: impl Fn(Span) -> Span,
) -> Result<(), Error> {
    match tag {
        "if" => {
            expression(body, toks, rest, &shift)?;
        }
        "for" => {
            // `for TARGET in EXPR` — find the `in` that separates them, then insist on an
            // expression after it.
            let at = toks[rest..]
                .iter()
                .position(|t| t.kind == Kind::Name && t.span.slice(body) == "in")
                .ok_or_else(|| {
                    err("expected token 'in'", shift(toks[rest.min(toks.len() - 1)].span))
                })?;
            expression(body, toks, rest + at + 1, &shift)?;
        }
        "macro" | "call" => signature(body, toks, rest, &shift)?,
        _ => {}
    }
    Ok(())
}

/// `{% macro m(a, b) %}`. The measured row is `{% macro m(a,) %}`: upstream's `parse_signature`
/// expects a name after every comma, so a trailing one is `expected token 'name', got ')'`.
fn signature(
    body: &str,
    toks: &[Token],
    rest: usize,
    shift: &impl Fn(Span) -> Span,
) -> Result<(), Error> {
    let mut i = rest;
    // `call` may carry its own signature or none; `macro` names the macro first.
    if toks.get(i).map(|t| t.kind) == Some(Kind::Name) {
        i += 1;
    }
    if toks.get(i).map(|t| t.kind) != Some(Kind::Lparen) {
        return Ok(());
    }
    i += 1;
    loop {
        match toks.get(i).map(|t| t.kind) {
            Some(Kind::Rparen) => return Ok(()),
            Some(Kind::Name) => i += 1,
            other => {
                let span = toks.get(i).map_or(Span { start: body.len(), end: body.len() }, |t| t.span);
                let got = match other {
                    Some(Kind::Rparen) | None => "end of statement block".to_string(),
                    Some(k) => format!("{k:?}").to_lowercase(),
                };
                return Err(err(format!("expected token 'name', got {got:?}"), shift(span)));
            }
        }
        // A default value, then either a comma or the closing paren.
        if toks.get(i).map(|t| t.kind) == Some(Kind::Assign) {
            let (_, next) = expression(body, toks, i + 1, shift)?;
            i = next;
        }
        match toks.get(i).map(|t| t.kind) {
            Some(Kind::Comma) => i += 1,
            Some(Kind::Rparen) => return Ok(()),
            _ => {
                let span = toks.get(i).map_or(Span { start: body.len(), end: body.len() }, |t| t.span);
                return Err(err("expected token ',' or ')'", shift(span)));
            }
        }
        // Upstream requires a name after a comma, so `(a,)` fails here rather than closing.
        if toks.get(i).map(|t| t.kind) == Some(Kind::Rparen) {
            return Err(err("expected token 'name', got ')'", shift(toks[i].span)));
        }
    }
}

/// The four target-naming tags all read their target with the same call — `parse_extends`,
/// `parse_include`, `parse_import` and `parse_from` each do `node.template =
/// self.parse_expression()`. Everything after it is modifiers this module does not need.
fn standalone_references(
    body: &str,
    toks: &[Token],
    rest: usize,
    tag: &str,
    shift: &impl Fn(Span) -> Span,
) -> Result<Vec<Reference>, Error> {
    let ref_tag = match tag {
        "extends" => RefTag::Extends,
        "include" => RefTag::Include,
        "import" => RefTag::Import,
        "from" => RefTag::From,
        "set" => {
            // `{% set x = EXPR %}` must reach an expression; the block form `{% set x %}` has
            // no `=` and is closed by `{% endset %}`.
            if let Some(at) = toks[rest..].iter().position(|t| t.kind == Kind::Assign) {
                expression(body, toks, rest + at + 1, shift)?;
            }
            return Ok(Vec::new());
        }
        "print" | "do" => {
            expression(body, toks, rest, shift)?;
            return Ok(Vec::new());
        }
        _ => return Ok(Vec::new()),
    };

    let (expr, next) = expression(body, toks, rest, shift)?;
    // `include EXPR [ignore missing] [with|without context]` — the modifiers follow the
    // expression as bare names, so reading them is a scan of what is left.
    let ignore_missing = ref_tag == RefTag::Include
        && toks[next..]
            .windows(2)
            .any(|w| slice(body, w[0].span) == "ignore" && slice(body, w[1].span) == "missing");
    // A dynamic name is not a reference. Upstream's `find_referenced_templates` yields `None`
    // for one, and staying silent is this ticket's rule too.
    //
    // A *list* is not dynamic: `{% include ['a.j2', 'b.j2'] %}` names both, tried in order,
    // and upstream reports both. A list with one dynamic element reports the literals it can
    // and stays quiet about the rest, which is also what upstream does.
    Ok(match expr.kind {
        ExprKind::Const(Const::Str(s)) => {
            vec![Reference { template: s, span: shift(expr.span), tag: ref_tag, ignore_missing }]
        }
        ExprKind::List(items) => items
            .into_iter()
            .filter_map(|e| match e.kind {
                ExprKind::Const(Const::Str(s)) => {
                    Some(Reference { template: s, span: shift(e.span), tag: ref_tag, ignore_missing })
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    })
}

fn expression(
    body: &str,
    toks: &[Token],
    at: usize,
    shift: &impl Fn(Span) -> Span,
) -> Result<(super::ast::Expr, usize), Error> {
    if toks.get(at).map(|t| t.kind).unwrap_or(Kind::Eof) == Kind::Eof {
        let span = toks
            .get(at)
            .map_or(Span { start: body.len(), end: body.len() }, |t| t.span);
        return Err(err("Expected an expression, got 'end of statement block'", shift(span)));
    }
    parser::expression_at(body, toks, at).map_err(|e| Error { span: shift(e.span), ..e })
}

/// Walk a whole template: every statement parsed, every block closed by its own end tag.
///
/// Returns the template references in document order. The nesting check is what makes an
/// unknown tag reportable at all — without it, `{% endfor %}` after an `{% if %}` is just a
/// tag nobody modelled.
pub fn check(src: &str, blocks: &[Block]) -> Result<Vec<Reference>, Error> {
    let mut refs = Vec::new();
    let mut stack: Vec<(String, Span)> = Vec::new();

    for b in blocks {
        if b.kind != template::Kind::Statement {
            continue;
        }
        match parse(src, b.inner)? {
            Stmt::Open { tag, references, .. } => {
                refs.extend(references);
                stack.push((tag, b.span));
            }
            Stmt::Standalone { references, .. } => refs.extend(references),
            Stmt::Continuation { tag } => {
                let allowed = continuation_openers(&tag).unwrap_or(&[]);
                match stack.last() {
                    Some((open, _)) if allowed.contains(&open.as_str()) => {}
                    _ => {
                        return Err(err(
                            format!("Encountered unknown tag '{tag}'."),
                            b.span,
                        ))
                    }
                }
            }
            Stmt::End { tag, opener } => match stack.last() {
                Some((open, _)) if *open == opener => {
                    stack.pop();
                }
                Some((open, _)) => {
                    return Err(err(
                        format!(
                            "Encountered unknown tag '{tag}'. Jinja was looking for the \
                             following tags: 'end{open}'. The innermost block that needs to \
                             be closed is '{open}'."
                        ),
                        b.span,
                    ))
                }
                None => {
                    return Err(err(format!("Encountered unknown tag '{tag}'."), b.span))
                }
            },
        }
    }

    if let Some((open, span)) = stack.last() {
        return Err(err(
            format!(
                "Unexpected end of template. Jinja was looking for the following tags: \
                 'end{open}'. The innermost block that needs to be closed is '{open}'."
            ),
            *span,
        ));
    }
    Ok(refs)
}

/// Every template this source references, or the reason it will not render.
pub fn references(src: &str, d: &Delimiters) -> Result<Vec<Reference>, Error> {
    // `document`, not `blocks`: a `#jinja2:` header can change every delimiter in the file,
    // and reading the body with the wrong ones invents tags that are not there. `d` is what
    // the `template:` module's parameters say, which the header then overrides.
    let (blocks, _) = template::document(src, d)?;
    check(src, &blocks)
}

/// The one thing a `.j2` file can be told today: it will not render. `Some(e)` is a template
/// `env.parse` also refuses, so the task that renders it fails on the target — and Ansible
/// does not find out until then, because a template is never parsed at playbook-parse time.
/// That gap is the whole reason to say it here.
///
/// **Silent when any Jinja extension is configured**, and not only for the tags one adds.
/// `jinja2.ext.Extension.preprocess` rewrites the source *before* lexing and may return
/// anything, so with an extension loaded no refusal of ours is safe to report — not just the
/// unknown-tag ones. Measured on ansible-core 2.21.2, both the ini key and the env var:
/// `{% break %}` is `Encountered unknown tag 'break'` by default and renders under
/// `jinja2.ext.loopcontrols`. `DEFAULT_JINJA2_EXTENSIONS` defaults to `[]`, so the default
/// case is the one that speaks.
pub fn will_not_render(src: &str, d: &Delimiters, extensions: &[String]) -> Option<Error> {
    if !extensions.is_empty() {
        return None;
    }
    references(src, d).err()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn refs(src: &str) -> Vec<Reference> {
        references(src, &Delimiters::default()).expect("renders")
    }

    fn names(src: &str) -> Vec<String> {
        refs(src).into_iter().map(|r| r.template).collect()
    }

    fn refusal(src: &str) -> String {
        references(src, &Delimiters::default()).expect_err("must not render").msg
    }

    /// T-040's nine measured rows. Every one is a template jinja2 3.1.6 refuses, and the whole
    /// argument for parsing all fourteen tags is that a four-tag reader takes all nine.
    #[test]
    fn the_nine_templates_jinja_refuses_are_refused() {
        let cases = [
            ("{% for x in xs %}{{ x }}", "endfor"),
            ("{% if a %}x{% endfor %}", "endfor"),
            ("{% forr x in xs %}{% endforr %}", "forr"),
            ("{% include 'a.j2' %}{% endif %}", "endif"),
            ("{% macro m(a,) %}{% endmacro %}", "name"),
            ("{% set x = %}", "expression"),
            ("{% for x in %}{% endfor %}", "expression"),
            ("{% raw %}{% include 'x.j2' %}", "raw"),
            ("{{ x }", "print statement"),
        ];
        for (src, needle) in cases {
            let msg = refusal(src);
            assert!(msg.contains(needle), "{src:?}: {msg:?} does not mention {needle:?}");
        }
    }

    /// The control for the row above, and the reason it is not vacuous: the *fixed* form of
    /// each is accepted. A reader that refused everything would pass the previous test.
    #[test]
    fn the_repaired_form_of_each_refusal_is_accepted() {
        for src in [
            "{% for x in xs %}{{ x }}{% endfor %}",
            "{% if a %}x{% endif %}",
            "{% for x in xs %}{% endfor %}",
            "{% include 'a.j2' %}",
            "{% macro m(a) %}{% endmacro %}",
            "{% set x = 1 %}",
            "{% raw %}{% include 'x.j2' %}{% endraw %}",
            "{{ x }}",
        ] {
            assert!(
                references(src, &Delimiters::default()).is_ok(),
                "{src:?} must render: {:?}",
                refusal(src)
            );
        }
    }

    /// The four target-naming tags. Each reads its target with the same `parse_expression`
    /// call upstream, which is why they are one code path here too.
    #[test]
    fn the_four_target_naming_tags_are_read() {
        assert_eq!(names("{% extends 'base.j2' %}"), ["base.j2"]);
        assert_eq!(names("{% include 'partials/header.j2' %}"), ["partials/header.j2"]);
        assert_eq!(names("{% import 'macros.j2' as m %}"), ["macros.j2"]);
        assert_eq!(names("{% from 'macros.j2' import a, b as c %}"), ["macros.j2"]);
        // The modifiers do not change the target.
        assert_eq!(names("{% include 'a.j2' ignore missing %}"), ["a.j2"]);
        // `ignore missing` is recorded, not just tolerated: an absent target is legal by
        // design there, and nothing may report it as a miss.
        assert!(refs("{% include 'a.j2' ignore missing %}")[0].ignore_missing);
        assert!(!refs("{% include 'a.j2' %}")[0].ignore_missing);
        assert!(!refs("{% extends 'a.j2' %}")[0].ignore_missing);
        // The modifier belongs to the include, not to the file: a second include on the same
        // line without it is still reportable.
        let two = refs("{% include 'a.j2' ignore missing %}{% include 'b.j2' %}");
        assert_eq!((two[0].ignore_missing, two[1].ignore_missing), (true, false));
        assert_eq!(names("{% include 'a.j2' with context %}"), ["a.j2"]);
        assert_eq!(names("{% include 'a.j2' without context %}"), ["a.j2"]);
    }

    /// A dynamic name is unresolvable, so it is not a reference. Upstream agrees — its
    /// `find_referenced_templates` yields `None` rather than a name.
    #[test]
    fn a_templated_include_name_stays_silent() {
        assert_eq!(names("{% include some_var %}"), Vec::<String>::new());
        assert_eq!(names("{% include 'a/' ~ name %}"), Vec::<String>::new());
        // The control: the literal spelling of the same tag *is* read, so silence is about
        // the name being dynamic rather than about `include` being unread.
        assert_eq!(names("{% include 'a.j2' %}"), ["a.j2"]);
    }

    /// A reference inside a `{% raw %}` or a comment is text, so it is not a reference. This
    /// is the join between the two modules and the one a `{%`-search gets wrong.
    #[test]
    fn a_reference_hidden_by_the_lexer_is_not_a_reference() {
        assert_eq!(names("{% raw %}{% include 'x.j2' %}{% endraw %}"), Vec::<String>::new());
        assert_eq!(names("{# {% include 'x.j2' %} #}"), Vec::<String>::new());
        assert_eq!(names("{{ '{% include \"x.j2\" %}' }}"), Vec::<String>::new());
    }

    /// The span points at the literal, which is what go-to-definition jumps from.
    #[test]
    fn a_reference_carries_the_span_of_its_literal() {
        let src = "head\n{% include 'partials/header.j2' %}\ntail";
        let r = &refs(src)[0];
        assert_eq!(r.tag, RefTag::Include);
        assert_eq!(&src[r.span.start..r.span.end], "'partials/header.j2'");
    }

    /// Nesting is what makes an unknown tag reportable: `{% endfor %}` is only wrong because
    /// the innermost open block is an `if`. Each of these names the block it could not close.
    #[test]
    fn a_block_must_be_closed_by_its_own_end_tag() {
        assert!(refusal("{% if a %}{% endfor %}").contains("'if'"));
        assert!(refusal("{% for x in xs %}{% endif %}").contains("'for'"));
        assert!(refusal("{% block b %}").contains("endblock"));
        assert!(refusal("{% macro m() %}").contains("endmacro"));
        // Nesting several deep still names the innermost.
        let msg = refusal("{% for x in xs %}{% if a %}{% endfor %}{% endif %}{% endfor %}");
        assert!(msg.contains("'if'"), "{msg}");
        // Correctly nested is fine.
        assert!(references("{% for x in xs %}{% if a %}{% endif %}{% endfor %}", &Delimiters::default()).is_ok());
    }

    /// `else` and `elif` continue a block rather than opening one, so they must not need
    /// closing — and must not be accepted with nothing open.
    #[test]
    fn a_continuation_needs_an_open_block_and_does_not_open_one() {
        assert!(references("{% if a %}x{% else %}y{% endif %}", &Delimiters::default()).is_ok());
        assert!(references("{% if a %}x{% elif b %}y{% endif %}", &Delimiters::default()).is_ok());
        assert!(references("{% for x in xs %}a{% else %}b{% endfor %}", &Delimiters::default()).is_ok());
        assert!(refusal("{% else %}").contains("else"));
        assert!(refusal("{% for x in xs %}{% elif b %}{% endfor %}").contains("elif"));
    }

    /// Every one of the fourteen is a tag this module knows, so none of them is reported as
    /// unknown. The list is the point of the ticket — a reader that models four cannot tell a
    /// tag it skipped from a tag that does not exist.
    #[test]
    fn all_fourteen_statement_forms_are_known() {
        for src in [
            "{% for x in xs %}{% endfor %}",
            "{% if a %}{% endif %}",
            "{% block b %}{% endblock %}",
            "{% extends 'a.j2' %}",
            "{% print x %}",
            "{% macro m() %}{% endmacro %}",
            "{% include 'a.j2' %}",
            "{% from 'a.j2' import x %}",
            "{% import 'a.j2' as m %}",
            "{% set x = 1 %}",
            "{% with a = 1 %}{% endwith %}",
            "{% autoescape true %}{% endautoescape %}",
            "{% call m() %}{% endcall %}",
            "{% filter upper %}{% endfilter %}",
        ] {
            let got = references(src, &Delimiters::default());
            assert!(got.is_ok(), "{src:?} must be known: {:?}", refusal(src));
        }
        // The control: a tag that really is not one of the fourteen is reported.
        assert!(refusal("{% frobnicate %}").contains("frobnicate"));
        assert!(refusal("{% endfrobnicate %}").contains("frobnicate"));
    }

    /// Every `.j2` under `demo/`, sorted by its path relative to the demo root.
    fn demo_templates() -> Vec<(String, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "j2") {
                    out.push(p);
                }
            }
        }
        let demo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo");
        let mut paths = Vec::new();
        walk(&demo, &mut paths);
        let mut out: Vec<(String, String)> = paths
            .into_iter()
            .map(|p| {
                let rel =
                    p.strip_prefix(&demo).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                (rel, std::fs::read_to_string(&p).expect("demo template"))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// The delimiters the server would read this template with — from the tasks that render
    /// it. Reading the demo any other way tests a path no user takes.
    fn delims(root: &std::path::Path, template: &std::path::Path) -> Delimiters {
        let sites = crate::resolve::render_sites(template, root, &crate::fs::StdFs);
        crate::resolve::delimiters_for_sites(&sites)
    }

    /// The `demo/` control T-040 asks for, and the pin rule 4 asks for: the fixture's
    /// comments are claims about what we extract, so the exact reference set of every demo
    /// template is asserted here. The right-hand column is
    /// `jinja2.meta.find_referenced_templates` on 3.1.6 run over these same files, including
    /// the `None` it yields for the dynamic `{% include tuning_file %}` — a name we must not
    /// invent.
    ///
    /// The fixture is also a working playbook: `templates_chain.yml` runs `ok=4 failed=0` on
    /// ansible-core 2.21.2, and the three same-named `shared.j2` files come out different,
    /// which is what makes `common.j2` the candidates problem rather than a lookup.
    #[test]
    fn demo_templates_name_exactly_what_jinja2_names() {
        let want: &[(&str, &[&str])] = &[
            ("roles/edge-cache/templates/shared.j2", &[]),
            ("roles/edge-proxy/templates/shared.j2", &[]),
            (
                "templates/app.conf.j2",
                &[
                    "base.conf.j2",
                    "macros.j2",
                    "macros.j2",
                    "partials/header.j2",
                    "optional.conf.j2",
                ],
            ),
            ("templates/base.conf.j2", &[]),
            ("templates/common.j2", &["shared.j2"]),
            // The missing-include fixture. Both spellings of the absent target are still
            // *references* — extraction says what the template names, and whether the file
            // exists is the resolver's question, not this one's.
            (
                "templates/broken_include.conf.j2",
                &["partials/nowhere.j2", "partials/nowhere.j2", "partials/header.j2"],
            ),
            ("templates/macros.j2", &[]),
            // No `#jinja2:` header: its delimiters come from the TASK that renders it. Read
            // with the defaults it is refused as `unknown tag 'notatag'` — the false positive
            // the call-site link exists to prevent — so the pin below reads every demo
            // template the way the server does, through `render_sites`.
            ("templates/module_delims.j2", &[]),
            // Read with its own `#jinja2:` delimiters. jinja2 is **not** the oracle for this
            // one: `find_referenced_templates` knows nothing about the header, so with the
            // default delimiters it calls the file `unknown tag 'notatag'` — the exact false
            // positive the header reader exists to prevent. Given the header's delimiters and
            // the header line removed, upstream agrees on `partials/header.j2`.
            ("templates/overridden.conf.j2", &["partials/header.j2"]),
            (
                "templates/optional.conf.j2",
                // Two names out of one list-valued include, because a list is not a dynamic
                // name. The dynamic include contributes nothing, and neither do the
                // `{% raw %}`, comment and `'%}'`-in-a-string rows.
                &["partials/site.j2", "partials/header.j2", "partials/absent.j2"],
            ),
            ("templates/partials/header.j2", &[]),
            ("templates/shared.j2", &[]),
        ];

        let demo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo");
        let found = demo_templates();
        // Non-zero is the whole point of a control: an empty demo tree passes everything
        // below by vacuity, and did until this fixture existed.
        assert!(!found.is_empty(), "demo has no .j2 files, so this control measures nothing");
        let names: Vec<&str> = found.iter().map(|(p, _)| p.as_str()).collect();
        let mut expected: Vec<&str> = want
            .iter()
            .map(|(p, _)| *p)
            .chain(["templates/broken.conf.j2", "templates/bad_header.conf.j2"])
            .collect();
        expected.sort_unstable();
        assert_eq!(names, expected, "the demo template set changed");

        let mut literal = 0;
        for (rel, src) in &found {
            // The two files that must not render, each refused for its own measured reason.
            if let Some(want_msg) = match rel.as_str() {
                "templates/broken.conf.j2" => Some("forr"),
                "templates/bad_header.conf.j2" => Some("nosuchkey"),
                _ => None,
            } {
                let msg = match references(src, &delims(&demo_root, &demo_root.join(rel))) {
                    Err(e) => e.msg,
                    Ok(r) => panic!("{rel} must not render, but we accepted it: {r:?}"),
                };
                assert!(msg.contains(want_msg), "{rel}: {msg}");
                continue;
            }
            let (_, theirs) = want.iter().find(|(p, _)| p == rel).expect("listed above");
            let ours = references(src, &delims(&demo_root, &demo_root.join(rel)))
                .unwrap_or_else(|e| panic!("{rel} must render, but we refuse it: {}", e.msg));
            let mine: Vec<&str> = ours.iter().map(|r| r.template.as_str()).collect();
            assert_eq!(mine, *theirs, "{rel}");
            literal += mine.len();
        }
        assert!(literal > 5, "only {literal} references — the fixture stopped exercising this");
    }

    /// The other claim the fixture makes, and rule 4 says a label is a claim: every `src:` in
    /// it now **navigates**, and none of them **warns**.
    ///
    /// This test used to assert the opposite — `src:` was not a reference at all — and it is
    /// what caught the change: adding `ReferenceKind::TemplateSrc` turned it red, which is
    /// how the demo's `NO HINT` comments got rewritten instead of quietly going stale.
    #[test]
    fn the_demo_src_values_navigate_and_do_not_warn() {
        let demo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo");
        let mut named = 0;
        for rel in [
            "templates_chain.yml",
            "roles/edge-proxy/tasks/main.yml",
            "roles/edge-cache/tasks/main.yml",
        ] {
            let text = std::fs::read_to_string(demo.join(rel)).expect("demo file");
            let nodes = crate::parse::Document::new(text).parse().expect("parses");
            let extracted = crate::references::extract(&nodes);
            for r in extracted.refs.iter().filter(|r| {
                r.kind == crate::references::ReferenceKind::TemplateSrc
            }) {
                assert!(r.value.ends_with(".j2"), "{rel}: {:?}", r.value);
                named += 1;
            }
        }
        assert_eq!(named, 7, "the demo renders seven templates by name");
        // The control: the walk still finds the references that were already working, so the
        // count above is about `src:` and not about a walk that found everything.
        let text = std::fs::read_to_string(demo.join("templates_chain.yml")).expect("demo file");
        let nodes = crate::parse::Document::new(text).parse().expect("parses");
        let extracted = crate::references::extract(&nodes);
        let roles: Vec<&str> = extracted
            .refs
            .iter()
            .filter(|r| r.kind == crate::references::ReferenceKind::Role)
            .map(|r| r.value.as_str())
            .collect();
        assert_eq!(roles, ["edge-proxy", "edge-cache"]);
    }

    /// Against `jinja2.meta.find_referenced_templates`, the oracle T-040 names. Upstream
    /// yields `None` for a dynamic name; this port yields no reference, so the comparison is
    /// on the literal names only, with the dynamic count asserted separately so "we found
    /// nothing" cannot pass as agreement.
    #[test]
    fn references_match_jinja2_meta_across_the_corpus() {
        let corpus = include_str!("template_corpus.jsonl");
        let (mut compared, mut literal, mut dynamic) = (0, 0, 0);

        for line in corpus.lines().filter(|l| !l.trim().is_empty()) {
            let row: Value = serde_json::from_str(line).expect("corpus line parses");
            let src = row["src"].as_str().expect("every row has a source");
            let Some(want) = row["refs"].as_array() else { continue };

            let theirs: Vec<String> =
                want.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
            let dyn_count = want.iter().filter(|v| v.is_null()).count();

            let Ok(ours) = references(src, &Delimiters::default()) else { continue };
            let mine: Vec<String> = ours.into_iter().map(|r| r.template).collect();
            assert_eq!(mine, theirs, "{src:?}");
            compared += 1;
            literal += theirs.len();
            dynamic += dyn_count;
        }
        assert!(compared > 30, "only {compared} templates compared");
        assert!(literal > 5, "only {literal} literal references — the oracle found nothing");
        assert!(dynamic > 0, "no dynamic names in the corpus, so silence is untested");
    }

    /// The same against the pinned trees, env-gated in the T-184 shape.
    #[test]
    #[ignore = "corpus gate: JINJA_TEMPLATE_CORPUS=<path> cargo test -p ansible-core --lib references_corpus_gate -- --ignored --nocapture"]
    fn references_corpus_gate() {
        let Ok(path) = std::env::var("JINJA_TEMPLATE_CORPUS") else { return };
        let text = std::fs::read_to_string(&path).expect("corpus is readable");
        let (mut compared, mut literal, mut dynamic, mut unrendered) = (0, 0, 0, 0);
        let mut differed = Vec::new();

        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let row: Value = serde_json::from_str(line).expect("corpus line parses");
            let src = row["src"].as_str().expect("every row has a source");
            let Some(want) = row["refs"].as_array() else { continue };
            let theirs: Vec<String> =
                want.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
            dynamic += want.iter().filter(|v| v.is_null()).count();

            match references(src, &Delimiters::default()) {
                Err(e) => {
                    // We refuse a template jinja2 parses: that is a false "will not render",
                    // the worst kind of answer this can give.
                    unrendered += 1;
                    differed.push(format!(
                        "REFUSED {:?} :: {}",
                        &src[..src.len().min(90)],
                        e.msg
                    ));
                }
                Ok(ours) => {
                    let mine: Vec<String> = ours.into_iter().map(|r| r.template).collect();
                    if mine != theirs {
                        differed.push(format!(
                            "{:?}\n     ours={mine:?}\n   jinja2={theirs:?}",
                            &src[..src.len().min(90)]
                        ));
                    }
                    compared += 1;
                    literal += theirs.len();
                }
            }
        }
        println!(
            "templates={compared} literal={literal} dynamic={dynamic} \
             falsely-refused={unrendered} differed={}",
            differed.len()
        );
        for d in differed.iter().take(25) {
            println!("  {d}");
        }
        assert!(compared > 0, "an empty corpus measures nothing");
        assert!(differed.is_empty(), "{} templates disagree", differed.len());
    }
}
