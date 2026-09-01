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
    /// `line_statement_prefix` — the rest of the line is a `{% … %}` body.
    LineStatement,
    /// `line_comment_prefix` — the rest of the line is a `{# … #}` body.
    LineComment,
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
    blocks_from(src, d, 0)
}

/// [`blocks`] starting at a byte offset, for a template whose `#jinja2:` header has already
/// been read. Upstream *removes* that line before lexing (`_jinja_bits.py:170`); starting the
/// scanner past it is the same split with spans that still index the file on disk, which is
/// what a diagnostic has to point at.
fn blocks_from(src: &str, d: &Delimiters, start: usize) -> Result<Vec<Block>, Error> {
    let mut s = Scanner { src, d, i: start, out: Vec::new(), trim_next_data: false };
    s.run()?;
    s.apply_whitespace_control();
    Ok(s.out)
}

/// `#jinja2:`, ansible-core's per-template delimiter override
/// (`_jinja_bits.py:76` for the marker, `:162-192` for the reader).
///
/// The prefix is matched with `startswith`, so it is the **first byte of the file** — a blank
/// line above it and it is ordinary text. Measured on 2.21.2: with the header on line 2 it is
/// rendered out verbatim and the delimiters it names never take effect.
///
/// Field precedence, also measured: **the header beats the `template:` module's parameters**
/// for the fields it names. Upstream is `dataclasses.replace(self, **override_kwargs)` with
/// `self` already carrying the module kwargs, so fields the header does not name keep the
/// module's value — hence `base` here rather than a fresh default.
#[derive(Debug, Clone)]
pub struct Header {
    /// Delimiters in effect for the body.
    pub delimiters: Delimiters,
    /// Where the body starts: one past the header's newline, or 0 with no header.
    pub body: usize,
}

/// Every key `TemplateOverrides` accepts (`_jinja_bits.py:105-116`). The six delimiters and
/// the two line prefixes change what we read; the last four change only the rendered bytes,
/// so they are validated as keys and otherwise ignored — see
/// `nothing_goes_missing_from_a_template_but_raw_tags_and_marked_whitespace` for why
/// `trim_blocks` in particular is not modelled.
const OVERRIDE_KEYS: &[&str] = &[
    "block_start_string",
    "block_end_string",
    "variable_start_string",
    "variable_end_string",
    "comment_start_string",
    "comment_end_string",
    "line_statement_prefix",
    "line_comment_prefix",
    "trim_blocks",
    "lstrip_blocks",
    "newline_sequence",
    "keep_trailing_newline",
];

const JINJA2_OVERRIDE: &str = "#jinja2:";

/// Read the `#jinja2:` header, if there is one. Every refusal below is measured verbatim on
/// ansible-core 2.21.2 — each is a template that fails at render on the target host, which is
/// exactly the class of fault worth saying at edit time.
pub fn header(src: &str, base: &Delimiters) -> Result<Header, Error> {
    if !src.starts_with(JINJA2_OVERRIDE) {
        return Ok(Header { delimiters: base.clone(), body: 0 });
    }
    let Some(eol) = src.find('\n') else {
        return Err(Error {
            msg: "Missing newline after '#jinja2:' override.".into(),
            span: Span { start: 0, end: src.len() },
            cause: Cause::Parse,
        });
    };
    let line = &src[JINJA2_OVERRIDE.len()..eol];
    let mut d = base.clone();
    let mut at = JINJA2_OVERRIDE.len();
    for pair in line.split(',') {
        let span = Span { start: at, end: at + pair.len() };
        at += pair.len() + 1;
        if pair.trim().is_empty() {
            return Err(Error {
                msg: "Empty '#jinja2:' override pair not allowed.".into(),
                span,
                cause: Cause::Parse,
            });
        }
        let Some(colon) = pair.find(':') else {
            return Err(Error {
                msg: format!(
                    "Missing key-value separator `:` in '#jinja2:' override pair {}.",
                    py_repr(pair)
                ),
                span,
                cause: Cause::Parse,
            });
        };
        let key = pair[..colon].trim();
        if !OVERRIDE_KEYS.contains(&key) {
            return Err(Error {
                msg: format!("Invalid '#jinja2:' override key {}.", py_repr(key)),
                span,
                cause: Cause::Parse,
            });
        }
        let raw = pair[colon + 1..].trim();
        let Some(value) = literal(raw) else {
            // `ast.literal_eval` is what upstream runs here; anything it would refuse is a
            // template that does not render, and we do not model the whole of Python.
            return Err(Error {
                msg: format!("Invalid value {} for '#jinja2:' override key {}.", py_repr(raw), py_repr(key)),
                span,
                cause: Cause::Parse,
            });
        };
        match key {
            "block_start_string" => d.block_start = value,
            "block_end_string" => d.block_end = value,
            "variable_start_string" => d.variable_start = value,
            "variable_end_string" => d.variable_end = value,
            "comment_start_string" => d.comment_start = value,
            "comment_end_string" => d.comment_end = value,
            "line_statement_prefix" => d.line_statement_prefix = Some(value),
            "line_comment_prefix" => d.line_comment_prefix = Some(value),
            // Render-shaping only; validated as a key and otherwise none of our business.
            _ => {}
        }
    }
    // `_post_validate`: the three start strings must all differ, or nothing can tell a tag
    // from a print from a comment.
    if d.block_start == d.variable_start
        || d.variable_start == d.comment_start
        || d.block_start == d.comment_start
    {
        return Err(Error {
            msg: "Block, variable and comment start strings must be different.".into(),
            span: Span { start: 0, end: eol },
            cause: Cause::Parse,
        });
    }
    Ok(Header { delimiters: d, body: eol + 1 })
}

/// Python's `repr` for the strings these messages quote, so the wording matches
/// ansible-core's byte for byte — the messages above are measured, and a message that is
/// nearly right is a message someone cannot search for.
fn py_repr(s: &str) -> String {
    if s.contains('\'') && !s.contains('\"') {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// The subset of `ast.literal_eval` a header value can be: a quoted string, or `None`. The
/// booleans and the newline literal belong to keys we ignore, so they are accepted and
/// discarded rather than parsed into anything.
fn literal(raw: &str) -> Option<String> {
    let b = raw.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        let inner = &raw[1..raw.len() - 1];
        // A quote inside the literal means it is not one simple string, which upstream would
        // read and we will not guess at.
        if inner.as_bytes().contains(&b[0]) {
            return None;
        }
        return Some(inner.to_string());
    }
    match raw {
        "None" | "True" | "False" => Some(String::new()),
        _ => None,
    }
}

/// [`blocks`] for a whole template file: read the `#jinja2:` header, then split the body with
/// whatever delimiters came out of it. `base` is what the `template:` module's parameters say,
/// or [`Delimiters::default`] when nothing does.
pub fn document(src: &str, base: &Delimiters) -> Result<(Vec<Block>, Delimiters), Error> {
    document_in(src, base, true)
}

/// [`document`] for a file whose grammar is **inherited** rather than its own.
///
/// Measured on ansible-core 2.21.2: a `#jinja2:` header in an *included* file is not honoured
/// — the header line is rendered out as ordinary text and the delimiters it names never apply.
/// `_extract_template_overrides` runs on the template the templar compiles, and an include
/// goes through jinja's loader instead, which knows nothing about the marker.
///
/// So `root` decides whether the header is read at all. Passing `true` for a file that is only
/// ever included reads it with a grammar Ansible never uses for it.
pub fn document_in(
    src: &str,
    base: &Delimiters,
    root: bool,
) -> Result<(Vec<Block>, Delimiters), Error> {
    if !root {
        return Ok((blocks(src, base)?, base.clone()));
    }
    let h = header(src, base)?;
    let bs = blocks_from(src, &h.delimiters, h.body)?;
    Ok((bs, h.delimiters))
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
                State::LineStatement => {
                    let n = self.d.line_statement_prefix.as_ref().map_or(0, String::len);
                    self.scan_line_statement(n)?
                }
                State::LineComment => {
                    let n = self.d.line_comment_prefix.as_ref().map_or(0, String::len);
                    self.scan_line_comment(n)?
                }
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
        let consider = |at: usize, len: usize, state: State, best: &mut Option<_>| {
            // Upstream sorts its rules by prefix length, longest first, so at one position the
            // longer delimiter wins — that is what keeps `##` a line comment when `#` is also
            // a line statement prefix.
            let better = match best {
                None => true,
                Some((b_at, b_len, _)) => at < *b_at || (at == *b_at && len > *b_len),
            };
            if better {
                *best = Some((at, len, state));
            }
        };
        for (lit, state) in [
            (&self.d.comment_start, State::Comment),
            (&self.d.block_start, State::Statement),
            (&self.d.variable_start, State::Expression),
        ] {
            if lit.is_empty() {
                continue;
            }
            if let Some(at) = rest.find(lit.as_str()) {
                consider(at, lit.len(), state, &mut best);
            }
        }
        // The two line rules are *additional*: with `line_statement_prefix` set, `{% %}` keeps
        // working and a prefix in the middle of a line is ordinary text. Measured on 2.21.2.
        if let Some(p) = &self.d.line_statement_prefix {
            if let Some((from, _)) = self.find_line_prefix(p, false) {
                consider(from - self.i, p.len(), State::LineStatement, &mut best);
            }
        }
        if let Some(p) = &self.d.line_comment_prefix {
            if let Some((from, _)) = self.find_line_prefix(p, true) {
                consider(from - self.i, p.len(), State::LineComment, &mut best);
            }
        }
        best.map(|(at, _, state)| (self.i + at, state))
    }

    /// The next occurrence of a line prefix that upstream's rule would match, as an absolute
    /// offset. The two rules differ and both are ported literally from `compile_rules`:
    ///
    /// - statement: `^[ \t\v]*<prefix>`
    /// - comment:   `(?:^|(?<=\S))[^\S\r\n]*<prefix>`
    ///
    /// So a statement prefix must open its line, with only blanks before it — which is why
    /// `not a statement: this line has a # in the middle` survives verbatim, measured. A
    /// comment prefix also matches after a non-blank, which is how `host = {{ h }} ## note`
    /// works.
    fn find_line_prefix(&self, prefix: &str, comment: bool) -> Option<(usize, usize)> {
        if prefix.is_empty() {
            return None;
        }
        let bytes = self.src.as_bytes();
        let mut from = self.i;
        while let Some(rel) = self.src[from..].find(prefix) {
            let at = from + rel;
            // Walk back over blanks. The vertical tab is in upstream's class for the
            // statement rule and not in the comment's; neither appears in a real template,
            // so the same walk serves both.
            let mut j = at;
            while j > 0 && matches!(bytes[j - 1], b' ' | b'\t' | 0x0b) {
                j -= 1;
            }
            let at_line_start = j == 0 || bytes[j - 1] == b'\n';
            // The comment rule's second branch: preceded by a non-blank on the same line,
            // with blanks between, or touching one directly.
            let prev_is_text = |k: usize| {
                k > 0 && !matches!(bytes[k - 1], b'\n' | b' ' | b'\t' | 0x0b)
            };
            let after_non_blank = comment && (prev_is_text(j) || prev_is_text(at));
            if at_line_start || after_non_blank {
                // The token starts at the blanks, not at the prefix: upstream's regex opens
                // with a horizontal-whitespace class, so the run before the marker belongs
                // to the tag and never reaches a `Data` block. Measured: a line-statement
                // prefix indented under a line of text leaves that line's `Data` ending at
                // its own newline, with the indent swallowed by the tag.
                return Some((j, at));
            }
            from = at + 1;
        }
        None
    }

    /// `line_statement_begin` runs to `\s*(\n|$)` — the rest of the line is the tag body,
    /// and there is no closing delimiter to find or to fail on.
    ///
    /// That end rule is greedier than it looks and it was measured, not assumed:
    /// `# for x in y\\n\\n\\nbody` leaves `Data` as `body`, because `\s*` swallows the whole
    /// blank run and backtracks to the last newline in it. Trailing spaces on the tag's own
    /// line go the same way, so they are not part of `inner`.
    fn scan_line_statement(&mut self, prefix_len: usize) -> Result<(), Error> {
        let start = self.i;
        let open = self.prefix_end(start, prefix_len);
        let eol = self.src[open..].find('\n').map_or(self.src.len(), |r| open + r);
        let body = self.src[open..eol].trim_end();
        let bytes = self.src.as_bytes();
        // Consume the blank run after the line, up to and including its last newline.
        let mut k = eol;
        let mut last_nl = None;
        while k < self.src.len() && bytes[k].is_ascii_whitespace() {
            if bytes[k] == b'\n' {
                last_nl = Some(k);
            }
            k += 1;
        }
        let after = last_nl.map_or(eol, |n| n + 1);
        self.out.push(Block {
            kind: Kind::Statement,
            span: Span { start, end: after },
            inner: Span { start: open, end: open + body.len() },
        });
        self.i = after;
        Ok(())
    }

    /// `line_comment_begin` runs to `(?=\n|$)`, and does **not** eat the newline —
    /// upstream's rule is a lookahead, so the line break survives into the following data.
    fn scan_line_comment(&mut self, prefix_len: usize) -> Result<(), Error> {
        let start = self.i;
        let open = self.prefix_end(start, prefix_len);
        let close = self.src[open..].find('\n').map_or(self.src.len(), |r| open + r);
        self.out.push(Block {
            kind: Kind::Comment,
            span: Span { start, end: close },
            inner: Span { start: open, end: close },
        });
        self.i = close;
        Ok(())
    }

    /// Where a line tag's body begins: past the blank run the token opened with, then past
    /// the prefix itself.
    fn prefix_end(&self, start: usize, prefix_len: usize) -> usize {
        let bytes = self.src.as_bytes();
        let mut k = start;
        while k < self.src.len() && matches!(bytes[k], b' ' | b'\t' | 0x0b) {
            k += 1;
        }
        (k + prefix_len).min(self.src.len())
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
            // Upstream's `raw_end` is a literal match for the endraw tag, never a parse:
            // inside a raw body there is no tag grammar, so a `{%` that opens nothing is
            // ordinary text. Reading it with `find_end` applied the tag rules — bracket depth
            // and strings — to data: on `{%s} text{% endraw %}` the `{` of the *real* endraw
            // takes the depth off zero, so its `%}` never counts as a terminator, `find_end`
            // came back empty and the raw was refused on the spot. `printf "x{%s}"` inside a
            // raw body is the shape that does it, common in shell and python templates.
            let Some(close) = self.raw_end(open) else {
                at = open;
                continue;
            };
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
    }

    /// Upstream's `raw_end` rule: the block start, an optional `-`/`+`, `endraw`, then the
    /// block end with an optional `-`. Returns the offset of that closing delimiter, so the
    /// caller reads both trim markers off the same offsets a parsed tag gave it.
    fn raw_end(&self, open: usize) -> Option<usize> {
        let ws = |s: &str| s.len() - s.trim_start().len();
        let mut i = open;
        if self.src[i..].starts_with(['-', '+']) {
            i += 1;
        }
        i += ws(&self.src[i..]);
        if !self.src[i..].starts_with("endraw") {
            return None;
        }
        i += "endraw".len();
        i += ws(&self.src[i..]);
        if self.src[i..].starts_with('-') {
            i += 1;
        }
        self.src[i..].starts_with(self.d.block_end.as_str()).then_some(i)
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
            // Byte comparison, not `self.src[i..]`. This loop walks one *byte* at a time,
            // so slicing the `str` here panics the moment `i` lands inside a multi-byte
            // character — `{{ café_port }}` was a hard crash of the whole request, on the
            // `.j2` path as much as in a YAML scalar. Comparing bytes is the same test with
            // no boundary requirement, and a multi-byte character can never match an ASCII
            // delimiter's first byte.
            if depth == 0 && bytes[i..].starts_with(end.as_bytes()) {
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

    /// **Why `trim_blocks` and `lstrip_blocks` are deliberately not modelled here**, which
    /// T-040 listed as unmeasured and now is not.
    ///
    /// Measured on ansible-core 2.21.2, byte-for-byte with `od`. Ansible's `template:` module
    /// defaults `trim_blocks: True` where stock jinja2 defaults it `False`, and both surfaces
    /// agree — a `set_fact` on the same string renders the same way, so this is the templar's
    /// default and not a module quirk:
    ///
    /// | source `A\n{% if true %}\nB\n{% endif %}\nC\n` | rendered |
    /// | --- | --- |
    /// | module default | `A\nB\nC\n` |
    /// | `trim_blocks: false` | `A\n\nB\n\nC\n` |
    /// | `lstrip_blocks` default (false) | leading spaces before a tag survive |
    /// | `lstrip_blocks: true` | they do not |
    ///
    /// Both settings drop bytes from the *rendered output*. Modelling them here would drop
    /// those bytes from our **spans**, which index the file the user is editing — a span that
    /// does not cover the source is a squiggle in the wrong place. So they are not modelled,
    /// and the golden corpus stays on a `trim_blocks=False` environment on purpose.
    ///
    /// That is only safe because neither setting changes anything we report. Probed against
    /// jinja2 3.1.6 over 20,010 templates (20,000 generated from statement/data/whitespace
    /// -control combinations, plus every `demo/*.j2`) under all four `(trim, lstrip)`
    /// combinations: **the parse verdict and the reference set never differed once**, while
    /// the block split differed on 13,286 of them — so the probe was thoroughly able to see a
    /// difference and there was none to see in the two things this crate answers.
    ///
    /// The invariant asserted below is what keeps it that way, and it is *not* "every byte is
    /// in a block" — that is already false, because a `-` marker shrinks the neighbouring
    /// `Data` span and this port follows jinja2 in doing so. The line is between whitespace
    /// dropped because the **template says to** (`{%-`, `-%}`, written right there) and
    /// whitespace dropped because of a **setting somewhere else**. So: a gap between blocks is
    /// either a raw tag, or whitespace next to an explicit `-` marker. Nothing else may go
    /// missing. Implement `trim_blocks` here — a newline swallowed after a plain `%}` — and
    /// this fails.
    #[test]
    fn nothing_goes_missing_from_a_template_but_raw_tags_and_marked_whitespace() {
        let d = Delimiters::default();
        let corpus = include_str!("template_corpus.jsonl");
        let mut checked = 0;
        let (mut raw_gaps, mut dash_gaps) = (0, 0);
        for line in corpus.lines().filter(|l| !l.trim().is_empty()) {
            let row: Value = serde_json::from_str(line).expect("corpus line parses");
            let src = row["src"].as_str().expect("every row has a source");
            let Ok(bs) = blocks(src, &d) else { continue };
            let mut at = 0usize;
            let mut prev: Option<&Block> = None;
            for b in &bs {
                assert!(b.span.start >= at, "blocks overlap in {src:?}");
                if b.span.start > at {
                    let gap = &src[at..b.span.start];
                    // A raw gap is the tag plus whatever whitespace its own `-` markers
                    // stripped, so trim before deciding which kind of gap this is.
                    let core = gap.trim();
                    let raw_tag = core.starts_with(&d.block_start)
                        && core.ends_with(&d.block_end)
                        && core.contains("raw");
                    if raw_tag {
                        raw_gaps += 1;
                    } else {
                        // Whitespace, and only because a `-` marker in the template asked
                        // for it — on the tag before the gap or the one after.
                        let after_dash = prev.is_some_and(|p| {
                            let s = &src[p.span.start..p.span.end];
                            s.ends_with(&format!("-{}", d.block_end))
                                || s.ends_with(&format!("-{}", d.variable_end))
                                || s.ends_with(&format!("-{}", d.comment_end))
                        });
                        let before_dash = {
                            let s = &src[b.span.start..b.span.end];
                            s.starts_with(&format!("{}-", d.block_start))
                                || s.starts_with(&format!("{}-", d.variable_start))
                                || s.starts_with(&format!("{}-", d.comment_start))
                        };
                        assert!(
                            gap.trim().is_empty() && (after_dash || before_dash),
                            "byte {at} of {src:?} went missing with no `-` marker asking                              for it: {gap:?}"
                        );
                        dash_gaps += 1;
                    }
                }
                at = b.span.end;
                prev = Some(b);
            }
            assert!(at <= src.len());
            checked += 1;
        }
        assert!(checked > 30, "only {checked} templates checked");
        // The controls: the corpus really does contain both shapes that leave a gap, so
        // "no illegal gap found" cannot pass by never meeting one.
        assert!(raw_gaps > 0, "no raw tag in the corpus, so that half is untested");
        assert!(dash_gaps > 0, "no `-` marker in the corpus, so that half is untested");
    }

    // ------------------------------------------------- line statements and line comments

    /// The sixth and seventh lexer states, against jinja2 3.1.6's own split.
    ///
    /// `line_statement_prefix` and `line_comment_prefix` are ordinary `TemplateOverrides`
    /// fields, so a `#jinja2:` header can turn them on and a `.j2` using them was, until this,
    /// read as one flat run of text — every include in it invisible. Worse than invisible in
    /// one shape: mixing is legal, so a `{% if %}` closed by a `# endif` looked like an
    /// unclosed block and earned a `template-syntax` error on a template that renders.
    ///
    /// Live-verified on ansible-core 2.21.2 through a `#jinja2:` header before the goldens
    /// were taken: `# for` / `# endfor` loops run, `##` comments vanish from the output, `{% %}`
    /// keeps working beside them, and a `#` in the middle of a line stays text.
    #[test]
    fn line_statements_and_comments_split_as_jinja2_splits_them() {
        let d = Delimiters {
            line_statement_prefix: Some("#".into()),
            line_comment_prefix: Some("##".into()),
            ..Delimiters::default()
        };
        let corpus = include_str!("line_corpus.jsonl");
        let mut checked = 0;
        for line in corpus.lines().filter(|l| !l.trim().is_empty()) {
            let row: Value = serde_json::from_str(line).expect("corpus line parses");
            let src = row["src"].as_str().expect("every row has a source");
            let want: Vec<(String, String)> = row["blocks"]
                .as_array()
                .expect("every row has blocks")
                .iter()
                .map(|b| {
                    (b[0].as_str().unwrap().to_string(), b[1].as_str().unwrap().to_string())
                })
                .collect();
            let got: Vec<(String, String)> = blocks(src, &d)
                .unwrap_or_else(|e| panic!("{src:?} refused: {}", e.msg))
                .into_iter()
                .map(|b| {
                    let kind = match b.kind {
                        Kind::Data => "data",
                        Kind::Comment => "comment",
                        Kind::Statement => "statement",
                        Kind::Expression => "expression",
                    };
                    (kind.to_string(), b.inner.slice(src).to_string())
                })
                .collect();
            assert_eq!(got, want, "{src:?}");
            checked += 1;
        }
        assert!(checked >= 18, "only {checked} rows");
    }

    /// The control that stops the two states being additive noise: with the prefixes **unset**
    /// — which is the default and every template in the wild — the same sources are plain
    /// text, and a `#` never means anything.
    #[test]
    fn without_the_prefixes_a_hash_is_just_a_hash() {
        let d = Delimiters::default();
        let src = "# for h in xs
host = {{ h }}   ## note
# endfor
";
        let got: Vec<Kind> = blocks(src, &d).expect("splits").into_iter().map(|b| b.kind).collect();
        assert_eq!(got, [Kind::Data, Kind::Expression, Kind::Data], "{got:?}");
        // And with them set, the same source is six — statement, data, expression, comment,
        // data, statement — so the assertion above is about the setting rather than about the
        // source having nothing in it.
        let on = Delimiters {
            line_statement_prefix: Some("#".into()),
            line_comment_prefix: Some("##".into()),
            ..Delimiters::default()
        };
        let kinds: Vec<Kind> =
            blocks(src, &on).expect("splits").into_iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Statement,
                Kind::Data,
                Kind::Expression,
                Kind::Comment,
                Kind::Data,
                Kind::Statement
            ]
        );
    }

    /// A `#jinja2:` header turning them on, end to end — the path a real template takes.
    #[test]
    fn a_header_can_turn_line_statements_on() {
        let src = "#jinja2: line_statement_prefix:\"#\"
# include \"partials/a.j2\"
";
        let (bs, d) = document(src, &Delimiters::default()).expect("splits");
        assert_eq!(d.line_statement_prefix.as_deref(), Some("#"));
        assert_eq!(bs.len(), 1);
        assert_eq!(bs[0].kind, Kind::Statement);
        // And it is a real reference, not just a block: this is the include that used to be
        // invisible.
        let refs = super::super::references(src, &Delimiters::default()).expect("renders");
        assert_eq!(refs.iter().map(|r| r.template.as_str()).collect::<Vec<_>>(), ["partials/a.j2"]);
    }

    /// The false positive this closes. Mixing the two spellings is legal — measured, a
    /// template with both renders — so a `{% if %}` closed by a `# endif` must not read as an
    /// unclosed block.
    #[test]
    fn a_block_opened_with_braces_can_be_closed_by_a_line_statement() {
        let d = Delimiters {
            line_statement_prefix: Some("#".into()),
            ..Delimiters::default()
        };
        let src = "{% if x %}
body
# endif
";
        assert!(super::super::references(src, &d).is_ok(), "{:?}", super::super::references(src, &d));
        // The control: read with the prefix unset — which is what we did before this — the
        // same template is a refusal, and that refusal was the false positive.
        let bare = super::super::references(src, &Delimiters::default());
        assert!(bare.is_err(), "the control no longer reproduces the false positive");
        assert!(bare.unwrap_err().msg.contains("endif"));
    }

    // ------------------------------------------------------------- the `#jinja2:` header

    fn doc(src: &str) -> Vec<(Kind, String)> {
        document(src, &Delimiters::default())
            .expect("splits")
            .0
            .into_iter()
            .map(|b| (b.kind, b.inner.slice(src).to_string()))
            .collect()
    }

    fn header_err(src: &str) -> String {
        document(src, &Delimiters::default()).expect_err("must be refused").msg
    }

    const VAR_HEADER: &str =
        "#jinja2: variable_start_string:\"[%\", variable_end_string:\"%]\"\n";
    const BLOCK_HEADER: &str = "#jinja2: block_start_string:\"<%\", block_end_string:\"%>\"\n";

    /// The header replaces the delimiters it names, and the defaults stop working — measured
    /// on ansible-core 2.21.2, where this exact file renders `WORLD and {{ not_a_var }}`:
    /// the `[% %]` pair is live and `{{ not_a_var }}` survives as literal text. That second
    /// half is the control, and a strong one — `not_a_var` is undefined, so had the default
    /// pair still been live the render would have failed rather than printed it.
    #[test]
    fn a_jinja2_header_replaces_the_delimiters_it_names() {
        let src = format!("{VAR_HEADER}[% name %] and {{{{ not_a_var }}}}\n");
        assert_eq!(
            doc(&src),
            [
                (Kind::Expression, " name ".to_string()),
                (Kind::Data, " and {{ not_a_var }}\n".to_string()),
            ]
        );
        // The header line itself is not a block: upstream removes it before lexing.
        assert!(!doc(&src).iter().any(|(_, s)| s.contains("#jinja2")));
    }

    /// The row that makes this worth doing at all. With block delimiters overridden,
    /// `{% notatag %}` is ordinary text — measured, the file renders `yes{% notatag %}`.
    /// Read with the default delimiters we would call `notatag` an unknown tag and put a red
    /// `template-syntax` squiggle on a template that works.
    #[test]
    fn an_overridden_block_delimiter_makes_a_stray_tag_ordinary_text() {
        let body = "<% if true %>yes<% endif %>{% notatag %}\n";
        let got = doc(&format!("{BLOCK_HEADER}{body}"));
        assert_eq!(got[0], (Kind::Statement, " if true ".to_string()));
        assert!(
            got.iter().any(|(k, s)| *k == Kind::Data && s.contains("{% notatag %}")),
            "{got:?}"
        );
        // The control: the same body without the header IS a refusal, so the header is doing
        // the work rather than the tag being harmless.
        assert!(super::super::references(body, &Delimiters::default()).is_err());
    }

    /// `startswith`, so the header is the first byte of the file. Measured: with a blank line
    /// above it, ansible renders the header out verbatim and `[% name %]` stays literal.
    #[test]
    fn a_jinja2_header_below_the_first_line_is_ordinary_text() {
        let got = doc(&format!("\n{VAR_HEADER}[% name %]\n"));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, Kind::Data);
        assert!(got[0].1.contains("#jinja2"), "{got:?}");
        assert!(got[0].1.contains("[% name %]"), "the override never took effect: {got:?}");
    }

    /// Spacing is whatever `split(',')` and `strip()` allow. Both spellings measured rendering
    /// `WORLD` on 2.21.2.
    #[test]
    fn header_pairs_tolerate_the_spacing_ansible_tolerates() {
        for src in [
            "#jinja2: variable_start_string:\"[%\", variable_end_string:\"%]\"\n[% name %]\n",
            "#jinja2:variable_start_string:\"[%\" ,variable_end_string:\"%]\"\n[% name %]\n",
        ] {
            assert_eq!(doc(src)[0], (Kind::Expression, " name ".to_string()), "{src:?}");
        }
    }

    /// Every way a header is rejected, each message measured verbatim on ansible-core 2.21.2
    /// as `Task failed: Syntax error in template: <this>`. Each is a template that fails at
    /// render on the target host, so each is worth saying at edit time.
    #[test]
    fn every_measured_header_refusal_is_refused_with_ansibles_words() {
        for (src, want) in [
            ("#jinja2: nosuchkey:\"x\"\nhi\n", "Invalid '#jinja2:' override key 'nosuchkey'."),
            (
                "#jinja2: variable_start_string\"[%\"\nhi\n",
                "Missing key-value separator `:` in '#jinja2:' override pair \
                 ' variable_start_string\"[%\"'.",
            ),
            (
                "#jinja2: variable_start_string:\"[%\", , variable_end_string:\"%]\"\nhi\n",
                "Empty '#jinja2:' override pair not allowed.",
            ),
            (
                "#jinja2: variable_start_string:\"{%\"\nhi\n",
                "Block, variable and comment start strings must be different.",
            ),
            (
                "#jinja2: variable_start_string:\"[%\"",
                "Missing newline after '#jinja2:' override.",
            ),
        ] {
            assert_eq!(header_err(src), want, "{src:?}");
        }
        // The control: the repaired form of each is accepted, so a reader that refused every
        // header would not pass this.
        for ok in [
            "#jinja2: variable_start_string:\"[%\"\nhi\n",
            "#jinja2: variable_start_string:\"[%\", variable_end_string:\"%]\"\nhi\n",
            "#jinja2: trim_blocks:False\nhi\n",
            "#jinja2: line_statement_prefix:\"#\"\nhi\n",
        ] {
            assert!(document(ok, &Delimiters::default()).is_ok(), "{ok:?}");
        }
    }

    /// A `#jinja2:` header is honoured only in the template the **task** names. In an
    /// included file it is inert.
    ///
    /// Measured on ansible-core 2.21.2: a root with default delimiters includes a partial
    /// carrying `variable_start_string:"[%"`. The rendered output shows the header line
    /// **verbatim as text**, `[% name %]` still literal, and `{{ name }}` rendered — so the
    /// partial was compiled in the root's grammar and its own header was never read.
    /// `_extract_template_overrides` runs on what the templar compiles; an include goes
    /// through jinja's loader, which knows nothing about the marker.
    #[test]
    fn an_included_files_own_header_is_inert() {
        let src = format!("{VAR_HEADER}[% name %] and {{{{ name }}}}
");
        let src = src.as_str();
        // As a root: the header applies, `[% %]` is the live pair, and the header line is not
        // a block.
        let (bs, d) = document_in(src, &Delimiters::default(), true).expect("splits");
        assert_eq!(d.variable_start, "[%");
        assert_eq!(bs[0].kind, Kind::Expression);
        assert_eq!(bs[0].inner.slice(src), " name ");

        // As an included file: the header is ordinary text and `{{ }}` is what renders.
        let (bs, d) = document_in(src, &Delimiters::default(), false).expect("splits");
        assert_eq!(d.variable_start, "{{");
        assert_eq!(bs[0].kind, Kind::Data);
        assert!(bs[0].inner.slice(src).contains("#jinja2"), "{:?}", bs[0].inner.slice(src));
        let live: Vec<&str> = bs
            .iter()
            .filter(|b| b.kind == Kind::Expression)
            .map(|b| b.inner.slice(src))
            .collect();
        assert_eq!(live, [" name "], "the `{{ }}` pair is the live one when inherited");
    }

    /// Spans still index the file on disk, header and all. Upstream removes the header line
    /// before lexing, so a port that lexed the remainder in isolation would report every
    /// diagnostic one line high — which is why `blocks_from` takes an offset instead of the
    /// caller slicing the string.
    #[test]
    fn a_header_does_not_shift_the_spans_of_the_body() {
        let src = format!("{VAR_HEADER}[% name %]\n");
        let (bs, _) = document(&src, &Delimiters::default()).expect("splits");
        let e = bs.iter().find(|b| b.kind == Kind::Expression).expect("one expression");
        assert_eq!(&src[e.span.start..e.span.end], "[% name %]");
        assert_eq!(src[..e.span.start].matches('\n').count(), 1, "line 2, not line 1");
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

    /// A `{%` inside a raw body opens nothing — upstream's `raw_end` is a literal match for
    /// the endraw tag, so everything until one is data. Found on `~/app/ansible`, where two
    /// templates wrap a script in `{% raw %}` and print a `{%s}` format inside it: reading
    /// that `{%` as a tag consumed the real `{% endraw %}` and called the raw unterminated.
    /// jinja2 3.1.6 lexes both of these as one `data` run between raw_begin and raw_end.
    #[test]
    fn a_stray_block_open_inside_a_raw_body_is_data() {
        assert_eq!(
            split("{% raw %}plain {%s} text{% endraw %}"),
            [(Kind::Data, "plain {%s} text".into())]
        );
        assert_eq!(
            split(r#"{% raw %}printf "x{%s} %.0f\n", s["k"]{% endraw %}"#),
            [(Kind::Data, r#"printf "x{%s} %.0f\n", s["k"]"#.into())]
        );
        // The control, and the half that keeps the fix honest: with no endraw anywhere the
        // raw is still unterminated, so this did not become "accept every raw".
        assert_eq!(err("{% raw %}plain {%s} text").msg, "Missing end of raw directive");
        // And the trim markers still reach the data on both sides of a real endraw, which is
        // what the rewritten matcher reads off the same offsets a parsed tag gave it.
        assert_eq!(split("a {% raw %} {%s} {%- endraw %} b")[0].1, "a ");
        assert_eq!(split("a {% raw %} {%s} {%- endraw %} b")[1].1, " {%s}");
        assert_eq!(split("a {% raw %} {%s} {% endraw -%} b")[2].1, "b");
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
