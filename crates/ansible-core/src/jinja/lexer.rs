//! The Jinja expression tokeniser — jinja2 3.1.6's `tag_rules`, ported.
//!
//! `lexer.py` builds six lexer states; this is the one shared by `block_begin` and
//! `variable_begin`, which is everything inside `{{ }}` or `{% %}` once the delimiter has
//! been consumed. Because [`crate::jinja`]'s surface starts *inside* an expression, no
//! delimiter is ever scanned for and the other five states are not here. T-040 adds them.
//!
//! Rule order is upstream's and it is load-bearing: whitespace, **float, integer**, name,
//! string, operator. Float before integer is what makes `1.5` one token; integer before name
//! is what makes `0b2` an integer `0` followed by the name `b2`, which is what jinja does.
//!
//! Two deliberate departures, both measured against jinja2 3.1.6:
//!
//! - **A [`Token`] carries a span, not a value.** Upstream decodes `'a\n'` and `0xff` while
//!   scanning. Deferring that keeps `Token` `Copy` and gives every node a byte range for free
//!   — the LSP needs ranges, and `lineno` (all upstream keeps) cannot produce one. The
//!   consequence is that a bad escape is an error from [`string_value`] rather than from
//!   [`tokens`]: `'\u26'` lexes here and fails upstream. Same rejection, later.
//! - **Identifiers are XID_Start/XID_Continue** via `unicode-ident`, where upstream matches a
//!   generated `\w`-ish table and then re-checks with `str.isidentifier()`. The accepted set
//!   is the same on everything measured; a lone `·` is rejected by both, upstream calling it
//!   "Invalid character in identifier" and this calling it an unexpected character.

// The parser is the only consumer and lands next under T-188; until then every item here is
// reachable from tests alone.
#![allow(dead_code)]

use crate::parse::Span;

/// Every token `tag_rules` can produce. Upstream emits one `operator` token and re-labels it
/// from a table; the table is folded in here so a `Kind` is the answer rather than a lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Name,
    Str,
    Int,
    Float,
    // Arithmetic.
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    Tilde,
    // Comparison. `!` alone is not an operator in Jinja — only `!=` is.
    Eq,
    Ne,
    Gt,
    Gteq,
    Lt,
    Lteq,
    Assign,
    // Punctuation.
    Dot,
    Comma,
    Colon,
    Semicolon,
    Pipe,
    Lparen,
    Rparen,
    Lbracket,
    Rbracket,
    Lbrace,
    Rbrace,
    /// Never produced by a rule; appended once so the parser always has something to look at.
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: Kind,
    pub span: Span,
}

/// A refusal, with the range to underline. Carried rather than discarded because T-040's
/// "this template will not render" diagnostic needs both; `classify` throws them away and
/// answers `Unknown`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub msg: String,
    pub span: Span,
}

/// `operators` from `lexer.py`, longest-first. Upstream sorts by `-len(x)` when building the
/// alternation; the order here *is* that sort, so `//` cannot be read as two `/`.
const OPERATORS: &[(&str, Kind)] = &[
    ("//", Kind::FloorDiv),
    ("**", Kind::Pow),
    ("==", Kind::Eq),
    ("!=", Kind::Ne),
    (">=", Kind::Gteq),
    ("<=", Kind::Lteq),
    ("+", Kind::Add),
    ("-", Kind::Sub),
    ("/", Kind::Div),
    ("*", Kind::Mul),
    ("%", Kind::Mod),
    ("~", Kind::Tilde),
    ("[", Kind::Lbracket),
    ("]", Kind::Rbracket),
    ("(", Kind::Lparen),
    (")", Kind::Rparen),
    ("{", Kind::Lbrace),
    ("}", Kind::Rbrace),
    (">", Kind::Gt),
    ("<", Kind::Lt),
    ("=", Kind::Assign),
    (".", Kind::Dot),
    (":", Kind::Colon),
    ("|", Kind::Pipe),
    (",", Kind::Comma),
    (";", Kind::Semicolon),
];

/// Tokenise one expression. The result always ends with [`Kind::Eof`].
///
/// Whitespace is dropped, exactly as upstream drops `TOKEN_WHITESPACE` through
/// `ignored_tokens` — which is why `hosts|length>0` and `hosts | length > 0` produce the same
/// stream, and why T-187 stops being a defect once anything reads this instead of the text.
pub fn tokens(src: &str) -> Result<Vec<Token>, Error> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;

    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let (kind, end) = if let Some(end) = scan_float(b, i) {
            (Kind::Float, end)
        } else if let Some(end) = scan_int(b, i) {
            (Kind::Int, end)
        } else if let Some(end) = scan_name(src, i) {
            (Kind::Name, end)
        } else if let Some(end) = scan_string(b, i) {
            (Kind::Str, end)
        } else if let Some((kind, end)) = scan_operator(src, i) {
            (kind, end)
        } else {
            let c = src[i..].chars().next().unwrap_or('\u{fffd}');
            return Err(Error {
                msg: format!("unexpected char {c:?}"),
                span: Span { start: i, end: i + c.len_utf8() },
            });
        };
        out.push(Token { kind, span: Span { start, end } });
        i = end;
    }

    out.push(Token { kind: Kind::Eof, span: Span { start: b.len(), end: b.len() } });
    Ok(out)
}

/// `(\d+_)*\d+` — digit runs joined by *single* underscores. `1_0` is one run; `1__0` stops
/// after the `1`, because the second `_` is where `\d+` has to start and does not.
fn digit_run(b: &[u8], mut i: usize, radix: u32) -> Option<usize> {
    let is_digit = |c: u8| (c as char).is_digit(radix);
    if i >= b.len() || !is_digit(b[i]) {
        return None;
    }
    while i < b.len() {
        if is_digit(b[i]) {
            i += 1;
        } else if b[i] == b'_' && i + 1 < b.len() && is_digit(b[i + 1]) {
            i += 2;
        } else {
            break;
        }
    }
    Some(i)
}

/// `integer_re`. The alternation order is upstream's, and it is why a leading zero splits a
/// literal: `0(_?0)*` takes the run of zeros and stops, so `01` is two integers and `007` is
/// `00` then `7`.
fn scan_int(b: &[u8], i: usize) -> Option<usize> {
    if i >= b.len() {
        return None;
    }
    // 0b / 0o / 0x, case-insensitive. `(_?[0-1])+` allows a leading underscore per digit.
    if b[i] == b'0' && i + 1 < b.len() {
        let radix = match b[i + 1] | 0x20 {
            b'b' => Some(2),
            b'o' => Some(8),
            b'x' => Some(16),
            _ => None,
        };
        if let Some(radix) = radix {
            let mut j = i + 2;
            let mut any = false;
            while j < b.len() {
                let d = if b[j] == b'_' { j + 1 } else { j };
                if d < b.len() && (b[d] as char).is_digit(radix) {
                    j = d + 1;
                    any = true;
                } else {
                    break;
                }
            }
            // `0b2` has no binary digit, so the prefix is not a prefix: fall through and the
            // decimal branch reads a lone `0`, leaving `b2` to be lexed as a name.
            if any {
                return Some(j);
            }
        }
    }
    match b[i] {
        b'1'..=b'9' => digit_run(b, i, 10),
        b'0' => {
            let mut j = i + 1;
            while j < b.len() {
                let d = if b[j] == b'_' { j + 1 } else { j };
                if d < b.len() && b[d] == b'0' {
                    j = d + 1;
                } else {
                    break;
                }
            }
            Some(j)
        }
        _ => None,
    }
}

/// `float_re`, including its `(?<!\.)` guard — without which `r.results[0].5` and, more to the
/// point, `foo.5` would take the `.5` as a float instead of a `dot` and an integer.
///
/// The two branches are upstream's alternation in order: an optional fraction *with* an
/// exponent, then a required fraction *without* one. `1.5e` matches the second and leaves `e`
/// to be a name, because the first needs digits after the `e` and has none.
fn scan_float(b: &[u8], i: usize) -> Option<usize> {
    if i > 0 && b[i - 1] == b'.' {
        return None;
    }
    let int_end = digit_run(b, i, 10)?;

    let exponent_from = |j: usize| -> Option<usize> {
        if j >= b.len() || b[j] | 0x20 != b'e' {
            return None;
        }
        let mut k = j + 1;
        if k < b.len() && (b[k] == b'+' || b[k] == b'-') {
            k += 1;
        }
        digit_run(b, k, 10)
    };
    let fraction_from = |j: usize| -> Option<usize> {
        if j < b.len() && b[j] == b'.' { digit_run(b, j + 1, 10) } else { None }
    };

    if let Some(frac_end) = fraction_from(int_end) {
        if let Some(end) = exponent_from(frac_end) {
            return Some(end);
        }
    }
    if let Some(end) = exponent_from(int_end) {
        return Some(end);
    }
    fraction_from(int_end)
}

/// `name_re` plus `str.isidentifier()`, as XID_Start/XID_Continue.
///
/// Reachable only when neither number rule matched, so a leading digit never arrives here —
/// `1a` is an integer and a name, not a rejected identifier.
fn scan_name(src: &str, i: usize) -> Option<usize> {
    let mut it = src[i..].char_indices();
    let (_, first) = it.next()?;
    if !(unicode_ident::is_xid_start(first) || first == '_') {
        return None;
    }
    let mut end = i + first.len_utf8();
    for (off, c) in it {
        if !unicode_ident::is_xid_continue(c) {
            break;
        }
        end = i + off + c.len_utf8();
    }
    Some(end)
}

/// `string_re`: `'([^'\\]*(?:\\.[^'\\]*)*)'` and its double-quoted twin, with `re.S` — so a
/// backslash may escape a newline and a raw newline may sit inside the quotes.
///
/// An unterminated quote is not an error here, it is simply not a string. Upstream reports it
/// the same way, by falling through to the operator rule and failing there: `'unterminated`
/// is "unexpected char `'`", never "unterminated string".
fn scan_string(b: &[u8], i: usize) -> Option<usize> {
    let quote = *b.get(i)?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let mut j = i + 1;
    while j < b.len() {
        if b[j] == b'\\' {
            // `\\.` with re.S — the escaped byte is consumed whatever it is. Stepping two
            // bytes is safe on UTF-8: a continuation byte is never `\\` or a quote.
            j += 2;
        } else if b[j] == quote {
            return Some(j + 1);
        } else {
            j += 1;
        }
    }
    None
}

fn scan_operator(src: &str, i: usize) -> Option<(Kind, usize)> {
    OPERATORS
        .iter()
        .find(|(text, _)| src[i..].starts_with(*text))
        .map(|(text, kind)| (*kind, i + text.len()))
}

/// The integer a [`Kind::Int`] token denotes.
///
/// Errors rather than wrapping on a literal too large for `i64`. Python has no such ceiling,
/// so this is a refusal where upstream succeeds — the honest shape for a limit we have and it
/// does not, and `Unknown` is the right answer for a condition we cannot represent.
pub fn int_value(src: &str, span: Span) -> Result<i64, Error> {
    let text = span.slice(src);
    let (radix, digits) = match text.get(..2).map(|p| p.to_ascii_lowercase()).as_deref() {
        Some("0b") => (2, &text[2..]),
        Some("0o") => (8, &text[2..]),
        Some("0x") => (16, &text[2..]),
        _ => (10, text),
    };
    i64::from_str_radix(&digits.replace('_', ""), radix).map_err(|_| Error {
        msg: format!("integer literal out of range: {text}"),
        span,
    })
}

/// The float a [`Kind::Float`] token denotes.
pub fn float_value(src: &str, span: Span) -> Result<f64, Error> {
    let text = span.slice(src);
    text.replace('_', "").parse().map_err(|_| Error {
        msg: format!("invalid float literal: {text}"),
        span,
    })
}

/// The string a [`Kind::Str`] token denotes, quotes stripped and escapes applied.
///
/// Upstream is `_normalize_newlines(body).encode("ascii", "backslashreplace")
/// .decode("unicode-escape")`, so the escape set is Python's, not Jinja's: an unrecognised
/// escape keeps its backslash (`'a\qb'` is five characters), and a backslash before a newline
/// is a line continuation.
///
/// `\N{NAME}` is refused. It needs the Unicode name database, which Rust does not ship, and
/// upstream resolves it — so this is a refusal where jinja succeeds, not a silent difference.
pub fn string_value(src: &str, span: Span) -> Result<String, Error> {
    let text = span.slice(src);
    let body = &text[1..text.len().saturating_sub(1)];
    let mut out = String::with_capacity(body.len());
    let mut it = body.chars().peekable();

    // `_normalize_newlines`: CRLF and a lone CR both become LF, before escapes are read.
    let push_normalized = |out: &mut String, c: char, it: &mut std::iter::Peekable<std::str::Chars<'_>>| {
        if c == '\r' {
            if it.peek() == Some(&'\n') {
                it.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    };

    while let Some(c) = it.next() {
        if c != '\\' {
            push_normalized(&mut out, c, &mut it);
            continue;
        }
        let Some(esc) = it.next() else {
            // A trailing backslash cannot happen: `scan_string` consumed the byte after it,
            // so the closing quote would have been eaten and the token would not exist.
            out.push('\\');
            break;
        };
        match esc {
            '\n' => {}
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
            }
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'a' => out.push('\u{7}'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\u{b}'),
            '0'..='7' => {
                // Up to three octal digits, the first already in hand.
                let mut v = esc as u32 - '0' as u32;
                for _ in 0..2 {
                    match it.peek() {
                        Some(&d @ '0'..='7') => {
                            v = v * 8 + (d as u32 - '0' as u32);
                            it.next();
                        }
                        _ => break,
                    }
                }
                out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
            }
            'x' => out.push(hex_escape(&mut it, 2, "\\xXX", span)?),
            'u' => out.push(hex_escape(&mut it, 4, "\\uXXXX", span)?),
            'U' => out.push(hex_escape(&mut it, 8, "\\UXXXXXXXX", span)?),
            'N' => {
                return Err(Error {
                    msg: "\\N{...} escapes are not supported".into(),
                    span,
                });
            }
            // Python leaves an unknown escape alone, backslash included.
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Ok(out)
}

fn hex_escape(
    it: &mut std::iter::Peekable<std::str::Chars<'_>>,
    width: usize,
    label: &str,
    span: Span,
) -> Result<char, Error> {
    let mut v: u32 = 0;
    for _ in 0..width {
        let d = it.next().and_then(|c| c.to_digit(16)).ok_or_else(|| Error {
            msg: format!("truncated {label} escape"),
            span,
        })?;
        v = v * 16 + d;
    }
    char::from_u32(v).ok_or_else(|| Error {
        msg: format!("invalid {label} escape"),
        span,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(kind, source text)` per token, Eof dropped — the shape jinja2's own tests print.
    fn lex(src: &str) -> Result<Vec<(Kind, &str)>, Error> {
        let toks = tokens(src)?;
        assert_eq!(toks.last().map(|t| t.kind), Some(Kind::Eof), "always terminated");
        Ok(toks[..toks.len() - 1].iter().map(|t| (t.kind, t.span.slice(src))).collect())
    }

    fn kinds(src: &str) -> Vec<Kind> {
        lex(src).expect("lexes").into_iter().map(|(k, _)| k).collect()
    }

    fn err(src: &str) -> Error {
        lex(src).expect_err("must not lex")
    }

    // ---------------------------------------------------------------- operators

    /// jinja2's `TestLexer::test_operators` walks its whole `operators` dict and asserts the
    /// token type of each. Ported wholesale, so a table that drifts from upstream fails here
    /// rather than somewhere downstream that only uses half of it.
    #[test]
    fn every_operator_in_upstream_s_table_lexes_to_its_own_kind() {
        let expected: &[(&str, Kind)] = &[
            ("+", Kind::Add),
            ("-", Kind::Sub),
            ("/", Kind::Div),
            ("//", Kind::FloorDiv),
            ("*", Kind::Mul),
            ("%", Kind::Mod),
            ("**", Kind::Pow),
            ("~", Kind::Tilde),
            ("[", Kind::Lbracket),
            ("]", Kind::Rbracket),
            ("(", Kind::Lparen),
            (")", Kind::Rparen),
            ("{", Kind::Lbrace),
            ("}", Kind::Rbrace),
            ("==", Kind::Eq),
            ("!=", Kind::Ne),
            (">", Kind::Gt),
            (">=", Kind::Gteq),
            ("<", Kind::Lt),
            ("<=", Kind::Lteq),
            ("=", Kind::Assign),
            (".", Kind::Dot),
            (":", Kind::Colon),
            ("|", Kind::Pipe),
            (",", Kind::Comma),
            (";", Kind::Semicolon),
        ];
        assert_eq!(expected.len(), OPERATORS.len(), "upstream's table has 26 entries");
        for (text, kind) in expected {
            assert_eq!(kinds(text), vec![*kind], "{text}");
        }
    }

    /// The whole point of sorting the table by length. Each of these would lex as two tokens
    /// under a naive single-character scan, and `a//b` in particular would become integer
    /// division reading as two divisions.
    #[test]
    fn two_character_operators_win_over_their_prefixes() {
        assert_eq!(kinds("a//b"), vec![Kind::Name, Kind::FloorDiv, Kind::Name]);
        assert_eq!(kinds("a**b"), vec![Kind::Name, Kind::Pow, Kind::Name]);
        assert_eq!(kinds("a!=b"), vec![Kind::Name, Kind::Ne, Kind::Name]);
        assert_eq!(kinds("a>=b"), vec![Kind::Name, Kind::Gteq, Kind::Name]);
        assert_eq!(kinds("a<=b"), vec![Kind::Name, Kind::Lteq, Kind::Name]);
        assert_eq!(kinds("a==b"), vec![Kind::Name, Kind::Eq, Kind::Name]);
    }

    /// `!=` is in the table; `!` is not. Measured: jinja2 3.1.6 rejects `!x` outright.
    #[test]
    fn a_lone_bang_is_not_an_operator() {
        assert_eq!(err("!x").msg, "unexpected char '!'");
        assert_eq!(err("a@b").span, Span { start: 1, end: 2 });
    }

    // ---------------------------------------------------------------- whitespace

    /// T-187 in one assertion. The two spellings differ only in `TOKEN_WHITESPACE`, which
    /// upstream drops through `ignored_tokens`, so nothing downstream can tell them apart.
    #[test]
    fn spacing_cannot_change_the_token_stream() {
        assert_eq!(kinds("hosts|length>0"), kinds("hosts | length > 0"));
        assert_eq!(kinds("a if b else c"), kinds("a\tif\nb  else\r\nc"));
    }

    #[test]
    fn spans_are_byte_ranges_into_the_source() {
        let src = "  hosts | length ";
        let t = tokens(src).expect("lexes");
        assert_eq!(t[0].span.slice(src), "hosts");
        assert_eq!(t[0].span, Span { start: 2, end: 7 });
        assert_eq!(t[2].span.slice(src), "length");
        assert_eq!(t.last().unwrap().span, Span { start: src.len(), end: src.len() });
    }

    /// Bytes, not chars — an accessor after a multi-byte literal must still slice correctly.
    #[test]
    fn spans_survive_non_ascii_earlier_in_the_expression() {
        let src = "'♨' ~ tail";
        let t = tokens(src).expect("lexes");
        assert_eq!(t[2].span.slice(src), "tail");
    }

    // ---------------------------------------------------------------- numbers

    /// Measured against jinja2 3.1.6, including the three nobody would guess: `01` is two
    /// integers, `1__0` is an integer and a name, and `0b2` is an integer and a name.
    #[test]
    fn integer_literals_match_upstream() {
        for (src, want) in [
            ("0", vec![(Kind::Int, "0")]),
            ("0b1010", vec![(Kind::Int, "0b1010")]),
            ("0o17", vec![(Kind::Int, "0o17")]),
            ("0xff", vec![(Kind::Int, "0xff")]),
            ("0B1010", vec![(Kind::Int, "0B1010")]),
            ("0XFF", vec![(Kind::Int, "0XFF")]),
            ("1_000", vec![(Kind::Int, "1_000")]),
            ("1_0", vec![(Kind::Int, "1_0")]),
            ("0_0", vec![(Kind::Int, "0_0")]),
            ("01", vec![(Kind::Int, "0"), (Kind::Int, "1")]),
            // `0(_?0)*` is greedy, so the run of zeros is one token and the `7` starts another.
            ("007", vec![(Kind::Int, "00"), (Kind::Int, "7")]),
            ("00", vec![(Kind::Int, "00")]),
            ("1__0", vec![(Kind::Int, "1"), (Kind::Name, "__0")]),
            ("1_000_", vec![(Kind::Int, "1_000"), (Kind::Name, "_")]),
            ("0b2", vec![(Kind::Int, "0"), (Kind::Name, "b2")]),
            // `(_?[0-1])+` puts the optional underscore *before* each digit, so one may sit
            // directly after the radix prefix — measured, all four spellings lex whole.
            ("0b_1", vec![(Kind::Int, "0b_1")]),
            ("0x_ff", vec![(Kind::Int, "0x_ff")]),
            ("0o_17", vec![(Kind::Int, "0o_17")]),
            ("0b1_0", vec![(Kind::Int, "0b1_0")]),
            // ...but an underscore with no digit after it is not a digit, so the prefix
            // collapses the same way `0b2` does.
            ("0x_", vec![(Kind::Int, "0"), (Kind::Name, "x_")]),
        ] {
            assert_eq!(lex(src).expect("lexes"), want, "{src}");
        }
    }

    #[test]
    fn float_literals_match_upstream() {
        for (src, want) in [
            ("1.5", vec![(Kind::Float, "1.5")]),
            ("1_0.0_1", vec![(Kind::Float, "1_0.0_1")]),
            ("5.0_1", vec![(Kind::Float, "5.0_1")]),
            ("2e3", vec![(Kind::Float, "2e3")]),
            ("1e-3", vec![(Kind::Float, "1e-3")]),
            ("1.5e2", vec![(Kind::Float, "1.5e2")]),
            // No digits after the `e`, so the exponent branch fails and the fraction branch
            // wins, leaving a name behind.
            ("1.5e", vec![(Kind::Float, "1.5"), (Kind::Name, "e")]),
            ("1.", vec![(Kind::Int, "1"), (Kind::Dot, ".")]),
            ("1.e3", vec![(Kind::Int, "1"), (Kind::Dot, "."), (Kind::Name, "e3")]),
            (".5", vec![(Kind::Dot, "."), (Kind::Int, "5")]),
        ] {
            assert_eq!(lex(src).expect("lexes"), want, "{src}");
        }
    }

    /// `float_re`'s `(?<!\.)` guard.
    ///
    /// `foo.5` alone does **not** exercise it — a bare `5` is not float-shaped, so the float
    /// rule declines on its own and the guard never runs. Deleting the guard leaves that
    /// assertion green, which is what makes it a decoration. The case that needs it is a
    /// float-shaped tail: without the guard `foo.5e3` reads as a name and the float `5e3`
    /// with the accessor swallowed, where jinja gives dot, integer, name (measured on 3.1.6).
    #[test]
    fn a_float_shaped_tail_after_a_dot_is_still_an_accessor() {
        assert_eq!(
            lex("foo.5e3").expect("lexes"),
            vec![(Kind::Name, "foo"), (Kind::Dot, "."), (Kind::Int, "5"), (Kind::Name, "e3")]
        );
        assert_eq!(
            lex("foo.5.5").expect("lexes"),
            vec![
                (Kind::Name, "foo"), (Kind::Dot, "."), (Kind::Int, "5"),
                (Kind::Dot, "."), (Kind::Int, "5")
            ]
        );
        // The shape this protects in practice — T-186's registered-result path.
        assert_eq!(
            lex("r.results[0].5e3").expect("lexes"),
            vec![
                (Kind::Name, "r"), (Kind::Dot, "."), (Kind::Name, "results"),
                (Kind::Lbracket, "["), (Kind::Int, "0"), (Kind::Rbracket, "]"),
                (Kind::Dot, "."), (Kind::Int, "5"), (Kind::Name, "e3")
            ]
        );
        // Kept because it is the ordinary spelling, not because it tests the guard.
        assert_eq!(
            lex("foo.5").expect("lexes"),
            vec![(Kind::Name, "foo"), (Kind::Dot, "."), (Kind::Int, "5")]
        );
    }

    #[test]
    fn numeric_values_decode() {
        let cases: &[(&str, i64)] = &[
            ("0", 0),
            ("0b1010", 10),
            ("0o17", 15),
            ("0xff", 255),
            ("0XFF", 255),
            ("1_000", 1000),
            ("1_0", 10),
        ];
        for (src, want) in cases {
            let t = tokens(src).expect("lexes");
            assert_eq!(int_value(src, t[0].span).expect("decodes"), *want, "{src}");
        }
        for (src, want) in [("1.5", 1.5), ("1_0.0_1", 10.01), ("2e3", 2000.0), ("1e-3", 0.001)] {
            let t = tokens(src).expect("lexes");
            assert_eq!(float_value(src, t[0].span).expect("decodes"), want, "{src}");
        }
    }

    /// Python has no integer ceiling and we do, so this refuses where jinja2 answers. Refusing
    /// is the point: the alternative is a wrapped value presented as the literal's meaning.
    #[test]
    fn an_integer_too_large_for_i64_is_refused_not_wrapped() {
        let src = "99999999999999999999";
        let t = tokens(src).expect("lexes");
        assert!(int_value(src, t[0].span).is_err());
    }

    // ---------------------------------------------------------------- strings

    #[test]
    fn both_quote_styles_lex() {
        assert_eq!(lex("'a'").expect("lexes"), vec![(Kind::Str, "'a'")]);
        assert_eq!(lex("\"d\"").expect("lexes"), vec![(Kind::Str, "\"d\"")]);
    }

    /// Adjacent strings stay two tokens here; it is `parse_primary` that folds them into one
    /// `Const`. Pinned so the folding lands in the parser and not quietly in the lexer.
    #[test]
    fn adjacent_strings_are_two_tokens() {
        assert_eq!(
            lex("'a' 'b'").expect("lexes"),
            vec![(Kind::Str, "'a'"), (Kind::Str, "'b'")]
        );
    }

    /// The half of T-188 that string surgery cannot get right: an operator inside quotes is
    /// text. `strip_strings` exists in `condition.rs` only because there was no lexer.
    #[test]
    fn operators_and_delimiters_inside_a_string_are_text() {
        for src in ["'a|b'", "'%}'", "'a > 0'", "'{{'"] {
            assert_eq!(lex(src).expect("lexes"), vec![(Kind::Str, src)], "{src}");
        }
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        assert_eq!(lex(r"'it\'s'").expect("lexes"), vec![(Kind::Str, r"'it\'s'")]);
        assert_eq!(lex(r#""a\"b""#).expect("lexes"), vec![(Kind::Str, r#""a\"b""#)]);
    }

    /// `re.S`, so a raw newline is allowed inside the quotes.
    #[test]
    fn a_raw_newline_inside_a_string_is_content() {
        assert_eq!(lex("'a\nb'").expect("lexes"), vec![(Kind::Str, "'a\nb'")]);
    }

    /// Upstream reports this as "unexpected char" at the opening quote rather than as an
    /// unterminated string, because `string_re` simply fails to match and the operator rule
    /// fails after it. Same shape here.
    #[test]
    fn an_unterminated_string_is_reported_at_its_opening_quote() {
        let e = err("'unterminated");
        assert_eq!(e.msg, "unexpected char '\\''");
        assert_eq!(e.span, Span { start: 0, end: 1 });
    }

    /// jinja2's `TestLexer::test_string_escapes` renders each of these back; without an
    /// evaluator the equivalent assertion is on the decoded literal.
    #[test]
    fn string_escapes_decode_as_python_does() {
        let cases: &[(&str, &str)] = &[
            (r"'a\nb'", "a\nb"),
            (r"'a\tb'", "a\tb"),
            (r"'a\rb'", "a\rb"),
            (r"'a\\b'", r"a\b"),
            (r"'it\'s'", "it's"),
            (r#""a\"b""#, "a\"b"),
            (r"'\x41'", "A"),
            (r"'\101'", "A"),
            (r"'\0'", "\0"),
            (r"'♨'", "\u{2668}"),
            (r"'\U0001f40d'", "\u{1f40d}"),
            ("'a♨b'", "a♨b"),
            // Python leaves an unrecognised escape alone, backslash and all.
            (r"'a\qb'", r"a\qb"),
            // A backslash before a newline is a line continuation.
            ("'a\\\nb'", "ab"),
        ];
        for (src, want) in cases {
            let t = tokens(src).expect("lexes");
            assert_eq!(string_value(src, t[0].span).expect("decodes"), *want, "{src}");
        }
    }

    /// `_normalize_newlines` runs before escapes, so a CRLF or a lone CR in the source becomes
    /// one LF in the value — while `\r` written as an escape stays a carriage return.
    #[test]
    fn raw_carriage_returns_normalize_but_the_escape_does_not() {
        for src in ["'a\r\nb'", "'a\rb'"] {
            let t = tokens(src).expect("lexes");
            assert_eq!(string_value(src, t[0].span).expect("decodes"), "a\nb", "{src:?}");
        }
        let src = r"'a\rb'";
        let t = tokens(src).expect("lexes");
        assert_eq!(string_value(src, t[0].span).expect("decodes"), "a\rb");
    }

    /// The C escapes upstream inherits from Python's `unicode-escape` but that no Ansible
    /// condition has ever needed. Measured on jinja2 3.1.6 rather than assumed from C.
    #[test]
    fn the_control_character_escapes_decode() {
        for (src, want) in
            [(r"'\a'", '\u{7}'), (r"'\b'", '\u{8}'), (r"'\f'", '\u{c}'), (r"'\v'", '\u{b}')]
        {
            let t = tokens(src).expect("lexes");
            assert_eq!(string_value(src, t[0].span).expect("decodes"), want.to_string(), "{src}");
        }
    }

    /// A backslash before a line ending is a continuation, and `_normalize_newlines` runs
    /// *first* — so all three spellings of a line ending collapse the same way. Measured:
    /// each of these renders `ab` upstream.
    #[test]
    fn a_backslash_before_any_line_ending_is_a_continuation() {
        for src in ["'a\\\nb'", "'a\\\r\nb'", "'a\\\rb'"] {
            let t = tokens(src).expect("lexes");
            assert_eq!(string_value(src, t[0].span).expect("decodes"), "ab", "{src:?}");
        }
    }

    /// Octal is one to three digits and `8` is not one of them, so `\8` keeps its backslash
    /// while `\777` is a character. Both measured.
    #[test]
    fn octal_escapes_stop_at_three_digits_and_at_the_digit_eight() {
        for (src, want) in [(r"'\777'", "\u{1ff}"), (r"'\8'", r"\8"), (r"'\1010'", "A0")] {
            let t = tokens(src).expect("lexes");
            assert_eq!(string_value(src, t[0].span).expect("decodes"), want, "{src}");
        }
    }

    /// Above the Unicode range. Upstream calls it "illegal Unicode character"; refused here
    /// too, which is the only honest answer for a codepoint that does not exist.
    #[test]
    fn a_codepoint_outside_unicode_is_refused() {
        let src = r"'\U00110000'";
        let t = tokens(src).expect("lexes");
        assert_eq!(
            string_value(src, t[0].span).expect_err("decode fails").msg,
            "invalid \\UXXXXXXXX escape"
        );
    }

    /// A divergence with no way around it: Python strings hold lone surrogates, so upstream
    /// decodes `\ud800` to one. A Rust `String` cannot represent it at all. Refused rather
    /// than substituted, so nothing downstream sees a character the author did not write.
    #[test]
    fn a_lone_surrogate_is_refused_where_python_keeps_it() {
        let src = r"'\ud800'";
        let t = tokens(src).expect("lexes");
        assert!(string_value(src, t[0].span).is_err());
    }

    /// Each width, both outcomes — a hex escape that completes and one that runs out of
    /// digits. `\x` and `\x4` are both "truncated" upstream, and so is `\u26`.
    #[test]
    fn hex_escapes_of_every_width_decode_or_refuse() {
        for (src, want) in
            [(r"'\x41'", "A"), (r"'\u0041'", "A"), (r"'\U00000041'", "A"), (r"'A'", "A")]
        {
            let t = tokens(src).expect("lexes");
            assert_eq!(string_value(src, t[0].span).expect("decodes"), want, "{src}");
        }
        for (src, label) in
            [(r"'\x'", "\\xXX"), (r"'\x4'", "\\xXX"), (r"'\u26'", "\\uXXXX"), (r"'\U0001'", "\\UXXXXXXXX")]
        {
            // Lexes, because decoding is deferred; the refusal comes from `string_value`.
            let t = tokens(src).expect("lexes");
            assert_eq!(
                string_value(src, t[0].span).expect_err("decode fails").msg,
                format!("truncated {label} escape"),
                "{src}"
            );
        }
    }

    /// Refused rather than guessed: jinja2 resolves `\N{HOT SPRINGS}` to `♨` from the Unicode
    /// name database, and Rust ships no such table.
    #[test]
    fn a_named_unicode_escape_is_refused_not_guessed() {
        let src = r"'\N{HOT SPRINGS}'";
        let t = tokens(src).expect("lexes");
        assert!(string_value(src, t[0].span).is_err());
    }

    // ---------------------------------------------------------------- identifiers

    /// jinja2's `TestLexer::test_name` table, re-measured at the *lexer* level. Upstream's
    /// version asserts that `env.from_string` raises, which conflates the two halves: `1a`
    /// and `a-` are perfectly good token streams that only the parser rejects. Ported as what
    /// the tokeniser actually does, measured case by case against jinja2 3.1.6.
    #[test]
    fn the_upstream_identifier_table_at_the_lexer_level() {
        // Valid, one name each.
        for src in ["foo", "föö", "き", "_", "ansible_facts", "\u{1885}", "\u{1886}", "\u{2118}", "\u{212e}"] {
            assert_eq!(lex(src).expect("lexes"), vec![(Kind::Name, src)], "{src}");
        }
        // Not identifier errors — two tokens, and it is the parser that objects.
        assert_eq!(lex("1a").expect("lexes"), vec![(Kind::Int, "1"), (Kind::Name, "a")]);
        assert_eq!(lex("a-").expect("lexes"), vec![(Kind::Name, "a"), (Kind::Sub, "-")]);
        // `·` is a continue character and not a start character.
        assert_eq!(lex("a\u{b7}").expect("lexes"), vec![(Kind::Name, "a\u{b7}")]);
        assert!(lex("\u{b7}").is_err());
        // An emoji is neither.
        assert!(lex("\u{1f40d}a").is_err());
        assert!(lex("a\u{1f40d}").is_err());
    }

    /// Keywords are names to the lexer — `parse_primary` is what turns `true` into a constant
    /// and `parse_compare` is what gives `not in` its meaning. Pinned so no keyword handling
    /// creeps down here, where it would have to guess at context.
    #[test]
    fn keywords_are_ordinary_names() {
        for src in ["true", "false", "none", "and", "or", "not", "in", "is", "if", "else"] {
            assert_eq!(lex(src).expect("lexes"), vec![(Kind::Name, src)], "{src}");
        }
        assert_eq!(kinds("not in"), vec![Kind::Name, Kind::Name]);
    }

    // ---------------------------------------------------------------- whole expressions

    /// The shapes `classify` is built around, each one currently recognised by a
    /// `strip_suffix` or a `split_once`. Nothing here asserts meaning — only that the stream
    /// a structural reader would need comes out whole.
    #[test]
    fn the_expressions_this_ticket_exists_for_tokenise() {
        assert_eq!(
            kinds("r.stdout | length > 0"),
            vec![Kind::Name, Kind::Dot, Kind::Name, Kind::Pipe, Kind::Name, Kind::Gt, Kind::Int]
        );
        assert_eq!(
            kinds("not (skip_x | default(false) | bool)"),
            vec![
                Kind::Name, Kind::Lparen, Kind::Name, Kind::Pipe, Kind::Name, Kind::Lparen,
                Kind::Name, Kind::Rparen, Kind::Pipe, Kind::Name, Kind::Rparen
            ]
        );
        assert_eq!(
            kinds("r.results[0].stdout"),
            vec![
                Kind::Name, Kind::Dot, Kind::Name, Kind::Lbracket, Kind::Int, Kind::Rbracket,
                Kind::Dot, Kind::Name
            ]
        );
        assert_eq!(
            kinds("mode in ['a', 'b']"),
            vec![Kind::Name, Kind::Name, Kind::Lbracket, Kind::Str, Kind::Comma, Kind::Str, Kind::Rbracket]
        );
        assert_eq!(kinds("x is not defined"), vec![Kind::Name; 4]);
    }

    #[test]
    fn containers_and_slices_tokenise() {
        assert_eq!(
            kinds("{'k': [1, 2]}"),
            vec![
                Kind::Lbrace, Kind::Str, Kind::Colon, Kind::Lbracket, Kind::Int, Kind::Comma,
                Kind::Int, Kind::Rbracket, Kind::Rbrace
            ]
        );
        assert_eq!(
            kinds("xs[1:2:-1]"),
            vec![
                Kind::Name, Kind::Lbracket, Kind::Int, Kind::Colon, Kind::Int, Kind::Colon,
                Kind::Sub, Kind::Int, Kind::Rbracket
            ]
        );
    }

    #[test]
    fn an_empty_expression_is_just_eof() {
        assert_eq!(tokens("").expect("lexes").len(), 1);
        assert_eq!(tokens("   ").expect("lexes")[0].kind, Kind::Eof);
    }

    // ---------------------------------------------------------------- differential

    /// Every token this lexer produces, against every token jinja2 3.1.6 produces, over 1025
    /// expressions harvested from jinja2's own test suite and this repo's `demo/` — plus a
    /// block of deliberately broken ones. Regenerate with:
    ///
    /// ```text
    /// python scripts/jinja_tokens.py <jinja2-sdist>/tests demo \
    ///     > crates/ansible-core/src/jinja/lexer_corpus.jsonl
    /// ```
    ///
    /// This is the test that matters. Coverage says every line ran; only the thing this was
    /// ported from can say the lines are right. Kinds *and* byte spans are compared, and a
    /// refusal must be a refusal on both sides at the same offset — so the bad paths are
    /// differential too, not just exercised.
    ///
    /// The corpus is newline-normalised because `tokeniter` rewrites its input before
    /// scanning and we deliberately do not; the CR spellings are pinned by hand above.
    #[test]
    fn the_whole_corpus_tokenises_exactly_as_jinja2_does() {
        let corpus = include_str!("lexer_corpus.jsonl");
        let mut checked = 0;
        let mut refusals = 0;

        for line in corpus.lines().filter(|l| !l.trim().is_empty()) {
            let row: serde_json::Value = serde_json::from_str(line).expect("corpus line parses");
            let src = row["src"].as_str().expect("every row has a source");
            let got = tokens(src);

            if let Some(expected) = row.get("toks").and_then(|t| t.as_array()) {
                let toks = got.unwrap_or_else(|e| panic!("{src:?} must lex, got {}", e.msg));
                let mine: Vec<(String, usize, usize)> = toks[..toks.len() - 1]
                    .iter()
                    .map(|t| (format!("{:?}", t.kind), t.span.start, t.span.end))
                    .collect();
                let theirs: Vec<(String, usize, usize)> = expected
                    .iter()
                    .map(|t| {
                        (
                            t[0].as_str().expect("kind").to_string(),
                            t[1].as_u64().expect("start") as usize,
                            t[2].as_u64().expect("end") as usize,
                        )
                    })
                    .collect();
                assert_eq!(mine, theirs, "{src:?}");
            } else {
                let err = got.expect_err("jinja2 refused this, so we must too");
                // Not every upstream refusal carries an offset — "Invalid character in
                // identifier" does not. Where one exists, ours has to point at the same byte.
                if let Some(at) = row["err_at"].as_u64() {
                    assert_eq!(err.span.start, at as usize, "{src:?} refused at the wrong byte");
                }
                refusals += 1;
            }
            checked += 1;
        }
        assert!(checked > 1000, "corpus shrank to {checked} — regenerated against the wrong tree?");
        assert!(refusals >= 16, "only {refusals} refusals — the bad paths left the corpus");
    }

    // ------------------------------------------------- paths `tokens` cannot reach

    // The three below are defensive branches that no token stream produces. They are covered
    // by calling into the module directly, because the alternative to a test is a branch
    // nobody has ever executed sitting in the middle of a scanner — and the decoders are
    // `pub`, so the parser can be handed a span this module did not mint.

    /// `tokens` never calls a scanner at or past the end, but the scanners are what the
    /// parser will lean on next; an out-of-range offset must decline, not index.
    #[test]
    fn a_scanner_at_the_end_of_input_declines() {
        assert_eq!(scan_int(b"", 0), None);
        assert_eq!(scan_int(b"x", 1), None);
        assert_eq!(scan_float(b"", 0), None);
        assert_eq!(scan_string(b"", 0), None);
        assert_eq!(scan_name("", 0), None);
        assert_eq!(scan_operator("", 0), None);
    }

    /// Every [`Kind::Float`] token this lexer emits parses, so the error arm is unreachable
    /// through [`tokens`]. It is not unreachable through the API: a span belonging to some
    /// other token must come back as an error rather than a panic or a zero.
    #[test]
    fn the_numeric_decoders_refuse_a_span_that_is_not_theirs() {
        let src = "name";
        let span = Span { start: 0, end: 4 };
        assert!(float_value(src, span).is_err());
        assert!(int_value(src, span).is_err());
    }

    /// `scan_string` consumes the byte after a backslash, so a body ending in one cannot come
    /// from a real token — `'a\'` is not a string at all, it is an unterminated quote. Hand
    /// the decoder that span anyway: the backslash is kept and nothing runs off the end.
    #[test]
    fn a_body_ending_in_a_backslash_keeps_it_rather_than_overrunning() {
        let src = r"'a\'";
        assert_eq!(scan_string(src.as_bytes(), 0), None, "not a token in the first place");
        assert_eq!(string_value(src, Span { start: 0, end: 4 }).expect("decodes"), "a\\");
    }

    /// The lexer has no notion of a delimiter, so `}}` is two braces and `{%` is a brace and a
    /// modulo. That is correct for this state and is why T-040 needs the other five, not a
    /// patch to this one.
    #[test]
    fn delimiters_are_not_special_in_this_state() {
        assert_eq!(kinds("}}"), vec![Kind::Rbrace, Kind::Rbrace]);
        assert_eq!(kinds("{%"), vec![Kind::Lbrace, Kind::Mod]);
    }
}
