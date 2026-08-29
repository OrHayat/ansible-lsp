//! The document grammar: splitting a template into the blocks `env.from_string` sees.
//!
//! This is the other half of [`super`]'s two compile paths. `when:` is one expression and
//! stops at [`super::parse`]; a `.j2` file — and, measured on ansible-core 2.21.2, **any
//! ordinary scalar** — goes through `env.from_string`, where `{% %}`, `{# #}` and `{{ }}` are
//! all legal. `msg: "{% for i in [1,2] %}{{ i }}{% endfor %}"` renders `12`, so a scalar is
//! this grammar and not the expression one (T-212).
//!
//! Everything here is lexical rather than syntactic, which is why it cannot be approximated by
//! searching for `{%`:
//!
//! - `{% raw %}` is a lexer *state*. An `{% include %}` inside one is literal text and is not a
//!   template reference.
//! - `{# … #}` is its own state, and a `{%` inside a comment is text.
//! - a `%}` inside brackets does not close the tag: `{% if x[1 %} ] %}` is one tag.
//! - neither does a `%}` inside a string: `{% if x == '%}' %}` is one tag.
//!
//! Ported from jinja2 3.1.6 `lexer.py`, whose `rules` table this mirrors state for state.

use crate::parse::Span;

use super::lexer::{Cause, Error};

/// What a template is read with. Not constants: the `template:` module takes
/// `variable_start_string` and friends as parameters, and a template may open with a
/// `#jinja2:` header line setting any of them. Both were measured overriding the defaults on
/// ansible-core 2.21.2 (T-040).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delimiters {
    pub block_start: String,
    pub block_end: String,
    pub variable_start: String,
    pub variable_end: String,
    pub comment_start: String,
    pub comment_end: String,
    pub line_statement_prefix: Option<String>,
    pub line_comment_prefix: Option<String>,
}

impl Default for Delimiters {
    fn default() -> Self {
        Self {
            block_start: "{%".into(),
            block_end: "%}".into(),
            variable_start: "{{".into(),
            variable_end: "}}".into(),
            comment_start: "{#".into(),
            comment_end: "#}".into(),
            line_statement_prefix: None,
            line_comment_prefix: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Literal text, including the body of a `{% raw %}`.
    Data,
    /// `{# … #}`.
    Comment,
    /// `{% … %}` — `inner` is the part a statement parser reads.
    Statement,
    /// `{{ … }}` — `inner` is one expression, for [`super::parse`].
    Expression,
}

/// One block of a template. `span` covers the delimiters, `inner` only the content between
/// them — the two differ for everything but [`Kind::Data`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub kind: Kind,
    pub span: Span,
    pub inner: Span,
}

/// Where a scan is.
enum State {
    Data,
    Comment,
    Statement,
    Expression,
}

struct Scanner<'a> {
    src: &'a str,
    d: &'a Delimiters,
    i: usize,
    out: Vec<Block>,
    /// Set by a `-%}` on a `raw`/`endraw` tag, which emits no block for the generic
    /// whitespace pass to work from.
    trim_next_data: bool,
}

/// Split `src` into blocks.
///
/// Refuses rather than guesses: an unclosed tag, comment or `{% raw %}` is an error, because
/// each is a template that will not render and saying so is the point (T-040).
pub fn blocks(src: &str, d: &Delimiters) -> Result<Vec<Block>, Error> {
    let mut s = Scanner { src, d, i: 0, out: Vec::new(), trim_next_data: false };
    s.run()?;
    s.apply_whitespace_control();
    Ok(s.out)
}

impl<'a> Scanner<'a> {
    fn run(&mut self) -> Result<(), Error> {
        while self.i < self.src.len() {
            let Some((at, state)) = self.next_delimiter() else {
                self.push_data(self.i, self.src.len());
                self.i = self.src.len();
                break;
            };
            self.push_data(self.i, at);
            self.i = at;
            match state {
                State::Comment => self.scan_comment()?,
                State::Statement => self.scan_statement()?,
                State::Expression => self.scan_expression()?,
                State::Data => unreachable!("`next_delimiter` never reports data"),
            }
        }
        Ok(())
    }

    /// `{%-` eats the whitespace before a tag and `-%}` the whitespace after it, so the two
    /// markers move where a [`Kind::Data`] block starts and ends. Upstream does this in the
    /// same place — its `block_end` token swallows the trimmed run — and a `Data` block that
    /// disappears entirely is dropped rather than left empty.
    fn apply_whitespace_control(&mut self) {
        for k in 0..self.out.len() {
            let b = self.out[k];
            if b.kind == Kind::Data {
                continue;
            }
            if self.opens_with_minus(&b) && k > 0 && self.out[k - 1].kind == Kind::Data {
                let prev = &mut self.out[k - 1];
                let text = self.src.get(prev.span.start..prev.span.end).unwrap_or("");
                prev.span.end = prev.span.start + text.trim_end().len();
                prev.inner = prev.span;
            }
            if self.closes_with_minus(&b) && k + 1 < self.out.len() {
                if self.out[k + 1].kind == Kind::Data {
                    let next = &mut self.out[k + 1];
                    let text = self.src.get(next.span.start..next.span.end).unwrap_or("");
                    next.span.start += text.len() - text.trim_start().len();
                    next.inner = next.span;
                }
            }
        }
        self.out.retain(|b| b.kind != Kind::Data || b.span.start < b.span.end);
    }

    /// Trim the trailing whitespace of the `Data` block just emitted, dropping it if that
    /// leaves it empty.
    fn trim_previous_data(&mut self) {
        let Some(prev) = self.out.last_mut() else { return };
        if prev.kind != Kind::Data {
            return;
        }
        let text = self.src.get(prev.span.start..prev.span.end).unwrap_or("");
        prev.span.end = prev.span.start + text.trim_end().len();
        prev.inner = prev.span;
        if prev.span.start >= prev.span.end {
            self.out.pop();
        }
    }

    fn opens_with_minus(&self, b: &Block) -> bool {
        let open_len = match b.kind {
            Kind::Comment => self.d.comment_start.len(),
            Kind::Statement => self.d.block_start.len(),
            Kind::Expression => self.d.variable_start.len(),
            Kind::Data => return false,
        };
        self.src[b.span.start + open_len..].starts_with('-')
    }

    fn closes_with_minus(&self, b: &Block) -> bool {
        let close_len = match b.kind {
            Kind::Comment => self.d.comment_end.len(),
            Kind::Statement => self.d.block_end.len(),
            Kind::Expression => self.d.variable_end.len(),
            Kind::Data => return false,
        };
        let at = b.span.end.saturating_sub(close_len);
        at > b.span.start && self.src[..at].ends_with('-')
    }
    /// The earliest delimiter at or after `self.i`. Ties go to the longest match, so a
    /// `variable_start` of `{{` cannot be read as a `block_start` of `{` (a real override:
    /// jinja's own tests use `Environment("<!--", "-->", "{", "}")`).
    fn next_delimiter(&self) -> Option<(usize, State)> {
        let rest = &self.src[self.i..];
        let mut best: Option<(usize, usize, State)> = None;
        for (lit, state) in [
            (&self.d.comment_start, State::Comment),
            (&self.d.block_start, State::Statement),
            (&self.d.variable_start, State::Expression),
        ] {
            if lit.is_empty() {
                continue;
            }
            if let Some(at) = rest.find(lit.as_str()) {
                let better = match &best {
                    None => true,
                    Some((b_at, b_len, _)) => at < *b_at || (at == *b_at && lit.len() > *b_len),
                };
                if better {
                    best = Some((at, lit.len(), state));
                }
            }
        }
        best.map(|(at, _, state)| (self.i + at, state))
    }

    /// The content of a tag with its whitespace-control markers removed. Upstream keeps `-`
    /// and `+` in the *delimiter* token, so a statement parser never sees them; `inner` here
    /// means the same thing.
    fn strip_markers(&self, mut inner: Span) -> Span {
        while inner.start < inner.end && matches!(self.src.as_bytes()[inner.start], b'-' | b'+') {
            inner.start += 1;
        }
        while inner.end > inner.start && matches!(self.src.as_bytes()[inner.end - 1], b'-' | b'+') {
            inner.end -= 1;
        }
        inner
    }
    fn push_data(&mut self, start: usize, mut end: usize) {
        let mut start = start;
        if std::mem::take(&mut self.trim_next_data) {
            let text = self.src.get(start..end).unwrap_or("");
            start += text.len() - text.trim_start().len();
        }
        if end < start {
            end = start;
        }
        if start < end {
            let span = Span { start, end };
            self.out.push(Block { kind: Kind::Data, span, inner: span });
        }
    }

    fn fail(&self, msg: &str, at: usize) -> Error {
        Error {
            msg: msg.to_string(),
            span: Span { start: at, end: self.src.len().min(at + 1) },
            cause: Cause::Lex,
        }
    }

    /// `{# … #}`. Its own state, so nothing inside it is a tag. jinja2 does not nest comments
    /// — the first `#}` closes it — and neither does this.
    fn scan_comment(&mut self) -> Result<(), Error> {
        let start = self.i;
        let open = start + self.d.comment_start.len();
        let Some(rel) = self.src[open..].find(self.d.comment_end.as_str()) else {
            return Err(self.fail("Missing end of comment tag", start));
        };
        let close = open + rel;
        self.out.push(Block {
            kind: Kind::Comment,
            span: Span { start, end: close + self.d.comment_end.len() },
            inner: self.strip_markers(Span { start: open, end: close }),
        });
        self.i = close + self.d.comment_end.len();
        Ok(())
    }

    fn scan_expression(&mut self) -> Result<(), Error> {
        let start = self.i;
        let open = start + self.d.variable_start.len();
        let Some(close) = self.find_end(open, &self.d.variable_end) else {
            return Err(self.fail("unexpected end of template, expected 'end of print statement'", start));
        };
        self.out.push(Block {
            kind: Kind::Expression,
            span: Span { start, end: close + self.d.variable_end.len() },
            inner: self.strip_markers(Span { start: open, end: close }),
        });
        self.i = close + self.d.variable_end.len();
        Ok(())
    }

    fn scan_statement(&mut self) -> Result<(), Error> {
        let start = self.i;
        let open = start + self.d.block_start.len();
        let Some(close) = self.find_end(open, &self.d.block_end) else {
            return Err(self.fail("unexpected end of template, expected 'end of statement block'", start));
        };
        let end = close + self.d.block_end.len();
        let inner = self.strip_markers(Span { start: open, end: close });

        // `raw` is handled here rather than by a statement parser because upstream handles it
        // here: it is a lexer state, so its body never becomes tokens at all.
        if tag_name(inner.slice(self.src)) == Some("raw") {
            let opens_minus = self.src[open..].starts_with('-');
            let closes_minus = self.src[..close].ends_with('-');
            if opens_minus {
                self.trim_previous_data();
            }
            return self.scan_raw(start, end, closes_minus);
        }
        self.out.push(Block { kind: Kind::Statement, span: Span { start, end }, inner });
        self.i = end;
        Ok(())
    }

    /// Everything up to `{% endraw %}` is literal text. An `{% include %}` in here is not a
    /// reference, and an unterminated one is a template that will not render.
    fn scan_raw(
        &mut self,
        raw_start: usize,
        body_start: usize,
        trim_body_start: bool,
    ) -> Result<(), Error> {
        self.trim_next_data = trim_body_start;
        let mut at = body_start;
        loop {
            let Some(rel) = self.src[at..].find(self.d.block_start.as_str()) else {
                return Err(self.fail("Missing end of raw directive", raw_start));
            };
            let tag_start = at + rel;
            let open = tag_start + self.d.block_start.len();
            let Some(close) = self.find_end(open, &self.d.block_end) else {
                return Err(self.fail("Missing end of raw directive", raw_start));
            };
            let name = tag_name(Span { start: open, end: close }.slice(self.src));
            if name == Some("endraw") {
                self.push_data(body_start, tag_start);
                // `{%- endraw %}` trims the end of the body, `{% endraw -%}` the start of
                // whatever follows. Neither tag becomes a block, so the generic pass cannot
                // reach them and both are applied here.
                if self.src[open..].starts_with('-') {
                    self.trim_previous_data();
                }
                self.trim_next_data = self.src[..close].ends_with('-');
                self.i = close + self.d.block_end.len();
                return Ok(());
            }
            at = close + self.d.block_end.len();
        }
    }

    /// The offset of `end` that actually terminates a tag opened at `from`.
    ///
    /// A `%}` is only a terminator at bracket depth zero and outside a string. Both cases are
    /// real: `{% if x == '%}' %}` is one tag, and so is a `%}` inside a list literal. Upstream
    /// calls the first a `balancing_stack`; the string half falls out of its `string` rule
    /// running before `block_end`.
    fn find_end(&self, from: usize, end: &str) -> Option<usize> {
        let bytes = self.src.as_bytes();
        let mut depth = 0i32;
        let mut i = from;
        while i < self.src.len() {
            let c = bytes[i];
            match c {
                b'\'' | b'"' => {
                    i = self.skip_string(i)?;
                    continue;
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                _ => {}
            }
            // A closing bracket that takes the depth negative is not ours to balance — it is
            // the first character of `}}`, or a stray. Either way stop counting down.
            if depth < 0 {
                depth = 0;
            }
            if depth == 0 && self.src[i..].starts_with(end) {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Past the string literal starting at `i`. `None` if it never closes, which makes the
    /// whole tag unterminated — the same answer upstream gives.
    fn skip_string(&self, i: usize) -> Option<usize> {
        let bytes = self.src.as_bytes();
        let quote = bytes[i];
        let mut j = i + 1;
        while j < self.src.len() {
            match bytes[j] {
                b'\\' => j += 2,
                c if c == quote => return Some(j + 1),
                _ => j += 1,
            }
        }
        None
    }
}

/// The first word of a tag body, lowercased by nothing — Jinja tag names are case-sensitive.
fn tag_name(inner: &str) -> Option<&str> {
    let t = inner.trim_start_matches(['-', '+']).trim_start();
    let name: &str = t.split(|c: char| !(c.is_alphanumeric() || c == '_')).next()?;
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn split(src: &str) -> Vec<(Kind, String)> {
        blocks(src, &Delimiters::default())
            .expect("splits")
            .into_iter()
            .map(|b| (b.kind, b.inner.slice(src).to_string()))
            .collect()
    }

    fn err(src: &str) -> Error {
        blocks(src, &Delimiters::default()).expect_err("must be refused")
    }

    // ------------------------------------------------------------------ the four traps

    /// `{% raw %}` is a lexer state, so an include inside one is text. This is the row that
    /// makes a `{%`-search wrong rather than merely imprecise: a search finds a template
    /// reference here and there is none.
    #[test]
    fn a_tag_inside_raw_is_text_and_not_a_statement() {
        assert_eq!(
            split("{% raw %}{% include 'x.j2' %}{% endraw %}tail"),
            [(Kind::Data, "{% include 'x.j2' %}".into()), (Kind::Data, "tail".into())]
        );
        // The control: the same include outside a raw *is* a statement, so the refusal above
        // is about `raw` and not about includes being invisible generally.
        assert_eq!(
            split("{% include 'x.j2' %}"),
            [(Kind::Statement, " include 'x.j2' ".into())]
        );
    }

    #[test]
    fn a_tag_inside_a_comment_is_text() {
        assert_eq!(
            split("{# {% include 'x.j2' %} #}"),
            [(Kind::Comment, " {% include 'x.j2' %} ".into())]
        );
    }

    /// `{% if x == '%}' %}` is one tag. Counting delimiters without tracking strings splits it
    /// into two and reads `' %}` as a second tag.
    #[test]
    fn an_end_delimiter_inside_a_string_does_not_close_the_tag() {
        for src in ["{% if x == '%}' %}ok{% endif %}", "{% if x == \"%}\" %}ok{% endif %}"] {
            let got = split(src);
            assert_eq!(got.len(), 3, "{src:?} -> {got:?}");
            assert_eq!(got[0].0, Kind::Statement);
            assert!(got[0].1.contains("=="), "{src:?}: {:?}", got[0].1);
            assert_eq!(got[1], (Kind::Data, "ok".into()));
        }
        assert_eq!(split("{{ '}}' }}"), [(Kind::Expression, " '}}' ".into())]);
    }

    #[test]
    fn an_end_delimiter_inside_brackets_does_not_close_the_tag() {
        assert_eq!(split("{{ {'a': 1} }}"), [(Kind::Expression, " {'a': 1} ".into())]);
        assert_eq!(
            split("{{ f(a, [1, 2], {'k': 'v'}) }}"),
            [(Kind::Expression, " f(a, [1, 2], {'k': 'v'}) ".into())]
        );
    }

    // ------------------------------------------------------------------ whitespace control

    /// `{%-` and `-%}` move where the data boundaries fall, so they change reported spans.
    /// Asserted on the text each `Data` block covers, which is what a span means.
    #[test]
    fn whitespace_control_moves_the_data_boundaries() {
        assert_eq!(
            split("a  {%- if x -%}  b  {%- endif -%}  c"),
            [
                (Kind::Data, "a".into()),
                (Kind::Statement, " if x ".into()),
                (Kind::Data, "b".into()),
                (Kind::Statement, " endif ".into()),
                (Kind::Data, "c".into()),
            ]
        );
        // Without the markers the same template keeps its whitespace, which is the control:
        // the trimming has to come from the markers and not from trimming everything.
        assert_eq!(
            split("a  {% if x %}  b  {% endif %}  c"),
            [
                (Kind::Data, "a  ".into()),
                (Kind::Statement, " if x ".into()),
                (Kind::Data, "  b  ".into()),
                (Kind::Statement, " endif ".into()),
                (Kind::Data, "  c".into()),
            ]
        );
    }

    /// The markers belong to the delimiter, not to the tag body — upstream keeps them in its
    /// `block_begin`/`block_end` tokens, so a statement parser never has to strip them.
    #[test]
    fn the_markers_are_not_part_of_the_tag_body() {
        assert_eq!(split("{%- if x -%}{%- endif -%}")[0].1, " if x ");
        assert_eq!(split("a {{- x -}} b")[1].1, " x ");
        assert_eq!(split("a {#- c -#} b")[1].1, " c ");
    }

    // ------------------------------------------------------------------ refusals

    /// Each of these is a template that will not render, and `env.parse` says so too — the
    /// corpus asserts that agreement. Being able to say it is the point of the whole state
    /// machine (T-040).
    #[test]
    fn a_template_that_cannot_render_is_refused() {
        assert_eq!(err("{# unclosed").msg, "Missing end of comment tag");
        assert_eq!(err("{% raw %}a").msg, "Missing end of raw directive");
        assert!(err("{{ x").msg.contains("end of print statement"));
        assert!(err("{% if x ").msg.contains("end of statement block"));
        // The control: the closed forms of all four are fine, so the refusals are about the
        // missing terminator rather than about the construct.
        for ok in ["{# c #}", "{% raw %}a{% endraw %}", "{{ x }}", "{% if x %}"] {
            assert!(blocks(ok, &Delimiters::default()).is_ok(), "{ok:?}");
        }
    }

    // ------------------------------------------------------------------ overridden delimiters

    /// Both mechanisms were measured overriding the defaults on ansible-core 2.21.2: the
    /// `template:` module's parameters, and a `#jinja2:` header. A hard-coded `{%` is wrong
    /// for either.
    #[test]
    fn delimiters_are_configuration_not_constants() {
        let d = Delimiters {
            variable_start: "[[".into(),
            variable_end: "]]".into(),
            ..Delimiters::default()
        };
        let src = "custom [[ name ]] and {{ not_a_var }}";
        let got: Vec<_> = blocks(src, &d)
            .expect("splits")
            .into_iter()
            .map(|b| (b.kind, b.inner.slice(src).to_string()))
            .collect();
        assert_eq!(
            got,
            [
                (Kind::Data, "custom ".into()),
                (Kind::Expression, " name ".into()),
                // `{{ }}` is literal text once it is not a delimiter — which is exactly what
                // the measured render produced, `{{ not_a_var }}` surviving verbatim.
                (Kind::Data, " and {{ not_a_var }}".into()),
            ]
        );
    }

    /// Jinja's own tests use `Environment("<!--", "-->", "{", "}")`, where `variable_start` is
    /// a prefix of nothing but `block_start` is one character. Longest match wins or `{{` is
    /// read as two blocks.
    #[test]
    fn the_longest_delimiter_wins_when_two_could_match() {
        let d = Delimiters {
            block_start: "{".into(),
            block_end: "}".into(),
            variable_start: "{{".into(),
            variable_end: "}}".into(),
            ..Delimiters::default()
        };
        let src = "{{ x }}";
        let got: Vec<_> = blocks(src, &d).expect("splits").into_iter().map(|b| b.kind).collect();
        assert_eq!(got, [Kind::Expression]);
    }

    // ------------------------------------------------------------------ the differential

    /// Every row is jinja2 3.1.6's own answer, from its own lexer. Regenerate with:
    ///
    /// ```text
    /// python scripts/jinja_blocks.py <jinja2-sdist>/tests demo \
    ///     > crates/ansible-core/src/jinja/template_corpus.jsonl
    /// ```
    ///
    /// Two oracles, because they answer different questions. `env.lex` gives the block split.
    /// `env.parse` gives whether the template renders — and it is the stricter of the two:
    /// `'{{ x'` comes back from the lexer as *no tokens and no error*, while `parse` calls it
    /// "unexpected end of template". This port follows `parse`, so the invariant asserted here
    /// is one-directional: **anything we refuse, `env.parse` must also refuse.** The converse
    /// does not hold yet — `{% if x %}` is a lexically complete tag and an unclosed block, and
    /// block nesting arrives with the statement forms.
    #[test]
    fn the_corpus_splits_exactly_as_jinja2_does() {
        let corpus = include_str!("template_corpus.jsonl");
        let (mut compared, mut refused) = (0, 0);

        for line in corpus.lines().filter(|l| !l.trim().is_empty()) {
            let row: Value = serde_json::from_str(line).expect("corpus line parses");
            let src = row["src"].as_str().expect("every row has a source");
            let parse_err = row["parse_err"].as_str();

            match blocks(src, &Delimiters::default()) {
                Err(e) => {
                    assert!(
                        parse_err.is_some(),
                        "{src:?}: we refuse with {:?}, jinja2 renders it",
                        e.msg
                    );
                    refused += 1;
                }
                Ok(got) => {
                    let Some(want) = row["blocks"].as_array() else { continue };
                    let ours: Vec<(String, String)> = got
                        .iter()
                        .map(|b| (kind_name(b.kind).to_string(), b.inner.slice(src).to_string()))
                        .collect();
                    let theirs: Vec<(String, String)> = want
                        .iter()
                        .map(|p| {
                            (
                                p[0].as_str().unwrap_or_default().to_string(),
                                p[1].as_str().unwrap_or_default().to_string(),
                            )
                        })
                        .collect();
                    assert_eq!(ours, theirs, "{src:?}");
                    compared += 1;
                }
            }
        }
        assert!(compared > 30, "only {compared} templates compared — corpus regenerated wrong?");
        assert!(refused > 0, "no refusals — the bad paths left the corpus");
    }

    /// The same comparison against a corpus generated from the pinned trees' `.j2` files,
    /// env-gated in the T-184 shape because those trees are never committed.
    #[test]
    #[ignore = "corpus gate: JINJA_TEMPLATE_CORPUS=<path> cargo test -p ansible-core --lib template_corpus_gate -- --ignored --nocapture"]
    fn template_corpus_gate() {
        let Ok(path) = std::env::var("JINJA_TEMPLATE_CORPUS") else { return };
        let text = std::fs::read_to_string(&path).expect("corpus is readable");
        let (mut compared, mut refused, mut differed) = (0, 0, Vec::new());

        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let row: Value = serde_json::from_str(line).expect("corpus line parses");
            let src = row["src"].as_str().expect("every row has a source");
            match blocks(src, &Delimiters::default()) {
                Err(e) => {
                    assert!(
                        row["parse_err"].as_str().is_some(),
                        "{:?}: we refuse with {:?}, jinja2 renders it",
                        &src[..src.len().min(80)],
                        e.msg
                    );
                    refused += 1;
                }
                Ok(got) => {
                    let Some(want) = row["blocks"].as_array() else { continue };
                    let ours: Vec<(String, String)> = got
                        .iter()
                        .map(|b| (kind_name(b.kind).to_string(), b.inner.slice(src).to_string()))
                        .collect();
                    if ours.len() != want.len()
                        || ours.iter().zip(want).any(|(o, w)| {
                            o.0 != w[0].as_str().unwrap_or_default()
                                || o.1 != w[1].as_str().unwrap_or_default()
                            })
                    {
                        differed.push(src[..src.len().min(120)].to_string());
                    }
                    compared += 1;
                }
            }
        }
        println!("templates={compared} refused={refused} differed={}", differed.len());
        for d in differed.iter().take(20) {
            println!("  DIFFERS {d:?}");
        }
        assert!(compared > 0, "an empty corpus measures nothing");
        assert!(differed.is_empty(), "{} templates split differently", differed.len());
    }

    fn kind_name(k: Kind) -> &'static str {
        match k {
            Kind::Data => "data",
            Kind::Comment => "comment",
            Kind::Statement => "statement",
            Kind::Expression => "expression",
        }
    }
}
