//! The YAML parser behind [`crate::parse::Document`], backed by `libyaml-safer` (a pure-Rust
//! libyaml port). Builds the [`Node`] tree from libyaml's event stream.
//!
//! Why libyaml and not strict YAML 1.2: the previous parser (saphyr) rejected multi-line
//! quoted scalars whose continuation lines aren't indented past their key — valid to Ansible
//! (PyYAML/libyaml), so real files like `roles/lustre-nvme-binding/tasks/_run.yml` went
//! unanalysed. libyaml accepts exactly what Ansible accepts. See T-036.
//!
//! This is the ONLY module allowed to touch `libyaml-safer`, so a version bump stays a
//! one-file change — the same isolation `parse.rs` once gave saphyr.

use std::collections::HashMap;

use libyaml_safer::{Event, EventData, Parser, ScalarStyle};

use crate::parse::{Node, Span};

/// Parse `text` into one [`Node`] per document. `None` if libyaml itself can't scan it —
/// i.e. broken for Ansible too, not merely stricter-than-Ansible.
pub fn parse_lenient(text: &str) -> Option<Vec<Node>> {
    let events = events(text)?;
    if undefined_alias(&events, text).is_some() {
        return None;
    }

    let mut docs = Vec::new();
    let mut i = 0;
    while i < events.len() {
        match events[i].data {
            EventData::DocumentStart { .. } => {
                // Anchors are per document.
                let mut anchors = HashMap::new();
                let (node, next) = build(&events, i + 1, text, &mut anchors);
                if let Some(node) = node {
                    docs.push(node);
                }
                i = next;
            }
            _ => i += 1,
        }
    }
    Some(docs)
}

/// The whole event stream, or `None` if libyaml can't scan the text.
fn events(text: &str) -> Option<Vec<Event>> {
    let mut parser = Parser::new();
    let mut input = text.as_bytes();
    parser.set_input_string(&mut input);

    let mut events: Vec<Event> = Vec::new();
    loop {
        let ev = parser.parse().ok()?;
        let done = matches!(ev.data, EventData::StreamEnd);
        events.push(ev);
        if done {
            break;
        }
    }
    Some(events)
}

/// A key written more than once in one mapping. Legal YAML: the constructor assigns the key
/// twice, so [`first`](Self::first)'s value is unreachable before any play sees the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateKey {
    pub key: String,
    /// The later occurrence — the one Ansible keeps, anchors its warning on, and the one
    /// [`Node::get`] returns.
    pub span: Span,
    /// The earlier occurrence, whose value is discarded.
    pub first: Span,
}

/// Every repeated mapping key in `text`, one entry per *later* occurrence — so a key written
/// three times yields two. Empty if libyaml can't scan the text at all.
///
/// A second walk of the same event stream rather than a second parse. It deliberately mirrors
/// [`build`]'s shape: the key/value pairing must be identical, or the rule would report a
/// duplicate the tree doesn't have.
pub fn duplicate_keys(text: &str) -> Vec<DuplicateKey> {
    let Some(events) = events(text) else { return Vec::new() };
    let mut out = Vec::new();
    let mut i = 0;
    while i < events.len() {
        match events[i].data {
            EventData::DocumentStart { .. } => i = scan_dupes(&events, i + 1, text, &mut out),
            _ => i += 1,
        }
    }
    out
}

/// Walk the node at event `i` recording duplicates; returns the index just past it.
fn scan_dupes(events: &[Event], i: usize, text: &str, out: &mut Vec<DuplicateKey>) -> usize {
    match &events[i].data {
        EventData::SequenceStart { .. } => {
            let mut j = i + 1;
            while !matches!(events[j].data, EventData::SequenceEnd) {
                j = scan_dupes(events, j, text, out);
            }
            j + 1
        }
        EventData::MappingStart { .. } => {
            // Per mapping, not per file: the same key in two sibling tasks is not a duplicate.
            let mut seen: Vec<(&str, Span)> = Vec::new();
            let mut j = i + 1;
            while !matches!(events[j].data, EventData::MappingEnd) {
                let key = match &events[j].data {
                    EventData::Scalar { value, .. } => Some((
                        value.as_str(),
                        unquote(
                            Span {
                                start: events[j].start_mark.index as usize,
                                end: events[j].end_mark.index as usize,
                            },
                            text,
                        ),
                    )),
                    // A non-scalar key (`? [a, b]`) has no name to compare.
                    _ => None,
                };
                let after_key = scan_dupes(events, j, text, out);
                let after_val = scan_dupes(events, after_key, text, out);
                if let Some((name, span)) = key {
                    match seen.iter().find(|(n, _)| *n == name) {
                        Some((_, first)) => out.push(DuplicateKey {
                            key: name.to_owned(),
                            span,
                            first: *first,
                        }),
                        None => seen.push((name, span)),
                    }
                }
                j = after_val;
            }
            j + 1
        }
        _ => i + 1,
    }
}

/// Where libyaml's scan failed, as a byte span, or `None` if it parses. Mirrors
/// `parse.rs`'s old `parse_error`: only fires when the file is broken for Ansible too.
pub fn parse_lenient_error(text: &str) -> Option<Span> {
    if let Some(events) = events(text) {
        return undefined_alias(&events, text);
    }
    let mut parser = Parser::new();
    let mut input = text.as_bytes();
    parser.set_input_string(&mut input);
    loop {
        match parser.parse() {
            Ok(ev) => {
                if matches!(ev.data, EventData::StreamEnd) {
                    return None;
                }
            }
            Err(e) => {
                let start = e
                    .problem_mark()
                    .map(|m| m.index as usize)
                    .unwrap_or(0)
                    .min(text.len());
                // Underline to end of the offending line, never an empty range.
                let line_end = text[start..]
                    .find('\n')
                    .map(|n| start + n)
                    .unwrap_or(text.len());
                return Some(Span {
                    start,
                    end: line_end.max((start + 1).min(text.len())),
                });
            }
        }
    }
}

/// The first alias naming an anchor not yet defined in its document, underlined to the end
/// of its line. libyaml scans it happily; Ansible's loader refuses the file, measured on
/// 2.21.3: "YAML parsing failed: Found undefined alias" (T-160). An anchor counts from the
/// start of its node, so `&a [*a]` is defined, and loads, as it does upstream.
fn undefined_alias(events: &[Event], text: &str) -> Option<Span> {
    let mut defined: Vec<&str> = Vec::new();
    for ev in events {
        match &ev.data {
            EventData::DocumentStart { .. } => defined.clear(),
            EventData::Scalar { anchor: Some(a), .. }
            | EventData::SequenceStart { anchor: Some(a), .. }
            | EventData::MappingStart { anchor: Some(a), .. } => defined.push(a),
            EventData::Alias { anchor } if !defined.contains(&anchor.as_str()) => {
                let start = (ev.start_mark.index as usize).min(text.len());
                let end = text[start..].find('\n').map_or(text.len(), |n| start + n);
                return Some(Span { start, end: end.max((start + 1).min(text.len())) });
            }
            _ => {}
        }
    }
    None
}

/// Each alias site and the anchored node it repeats, by span, in document order (T-160).
/// For go-to-definition and related information; no rule reads it, since the tree already
/// holds the substituted value.
pub fn aliases(text: &str) -> Vec<(Span, Span)> {
    let Some(docs) = events(text) else { return Vec::new() };
    let mut out = Vec::new();
    let mut i = 0;
    while i < docs.len() {
        if matches!(docs[i].data, EventData::DocumentStart { .. }) {
            let mut anchors = HashMap::new();
            let (_, next) = build_recording(&docs, i + 1, text, &mut anchors, &mut out);
            i = next;
        } else {
            i += 1;
        }
    }
    out
}

/// Build one node starting at event `i`; returns it and the index just past it.
fn build(events: &[Event], i: usize, text: &str, anchors: &mut HashMap<String, Node>) -> (Option<Node>, usize) {
    build_recording(events, i, text, anchors, &mut Vec::new())
}

/// [`build`], also recording each alias site with its anchor's span.
///
/// An alias becomes a copy of the anchored node, as Ansible's loader resolves it before any
/// play sees the file. A sequence or mapping takes the alias site's span, so a fault in the
/// value lands where the value is used: `empty: &empty []` is a fine variable and
/// `hosts: *empty` is the error. Upstream reports the anchor instead. A scalar keeps the
/// anchor's span, because offsets into its value map through that span. Its children keep
/// theirs too, so text sliced through any of them is the text they were parsed from. An
/// anchor is stored only once its node is complete, so `&a [*a]` stays a positioned `Other`
/// rather than a copy of itself.
fn build_recording(
    events: &[Event],
    i: usize,
    text: &str,
    anchors: &mut HashMap<String, Node>,
    sites: &mut Vec<(Span, Span)>,
) -> (Option<Node>, usize) {
    let (node, next) = build_node(events, i, text, anchors, sites);
    let anchor = match &events[i].data {
        EventData::Scalar { anchor, .. }
        | EventData::SequenceStart { anchor, .. }
        | EventData::MappingStart { anchor, .. } => anchor.as_ref(),
        _ => None,
    };
    if let (Some(a), Some(n)) = (anchor, &node) {
        anchors.insert(a.clone(), n.clone());
    }
    (node, next)
}

fn build_node(
    events: &[Event],
    i: usize,
    text: &str,
    anchors: &mut HashMap<String, Node>,
    sites: &mut Vec<(Span, Span)>,
) -> (Option<Node>, usize) {
    let ev = &events[i];
    let raw = Span {
        start: ev.start_mark.index as usize,
        end: ev.end_mark.index as usize,
    };
    match &ev.data {
        // A plain scalar with no content is YAML null, not an empty string. Both arrive here
        // as `value: ""` and the marks are the only place the difference survives: `""` spans
        // its two quotes, a null spans nothing. It matters because `when:` and `when: ""` are
        // absence and a fatal error respectively (T-117).
        EventData::Scalar { value, .. } if value.is_empty() && raw.start == raw.end => {
            (Some(Node::Null { span: raw }), i + 1)
        }
        EventData::Scalar { value, style, .. } => {
            // libyaml's mark starts at the node's `&anchor` / `!tag`, not at the value, and
            // every offset into the value is added to this span's start.
            let raw = past_properties(raw, text);
            let lines = match style {
                ScalarStyle::Literal | ScalarStyle::Folded => block_lines(value, raw, text),
                _ => flow_lines(value, raw, text, *style),
            };
            (Some(Node::Scalar { value: value.clone(), span: unquote(raw, text), lines }), i + 1)
        }
        EventData::SequenceStart { .. } => {
            let mut items = Vec::new();
            let mut j = i + 1;
            while !matches!(events[j].data, EventData::SequenceEnd) {
                let (item, next) = build_recording(events, j, text, anchors, sites);
                if let Some(item) = item {
                    items.push(item);
                }
                j = next;
            }
            let span = Span { start: raw.start, end: events[j].end_mark.index as usize };
            (Some(Node::Sequence { items, span }), j + 1)
        }
        EventData::MappingStart { .. } => {
            let mut entries = Vec::new();
            let mut j = i + 1;
            while !matches!(events[j].data, EventData::MappingEnd) {
                let (key, after_key) = build_recording(events, j, text, anchors, sites);
                let aliased_before = sites.len();
                let (val, after_val) = build_recording(events, after_key, text, anchors, sites);
                // A `<<` merge key stays opaque when its value comes through an alias, as it
                // was before aliases were substituted. Measured on 2.21.3, `- <<: *tasks` with
                // `tasks: &tasks [one, two]` merges the list into a single task, the first
                // entry winning, and runs only `one`; read as a `<<` key holding a list, it is
                // a false "unexpected parameter type". Modelling the merge itself is not done.
                let val = match (&key, val) {
                    (Some(k), Some(v)) if k.as_str() == Some("<<") && sites.len() > aliased_before => {
                        Some(Node::Other { span: v.span() })
                    }
                    (_, v) => v,
                };
                if let (Some(key), Some(val)) = (key, val) {
                    entries.push((key, val));
                }
                j = after_val;
            }
            let span = Span { start: raw.start, end: events[j].end_mark.index as usize };
            (Some(Node::Mapping { entries, span }), j + 1)
        }
        EventData::Alias { anchor } => match anchors.get(anchor) {
            Some(node) => {
                sites.push((raw, node.span()));
                let node = match node.clone() {
                    Node::Sequence { items, .. } => Node::Sequence { items, span: raw },
                    Node::Mapping { entries, .. } => Node::Mapping { entries, span: raw },
                    other => other,
                };
                (Some(node), i + 1)
            }
            // A self-reference, still being built: positioned, and never recursed into.
            None => (Some(Node::Other { span: raw }), i + 1),
        },
        // Anything unexpected: a positioned Other, so spans stay sane.
        _ => (Some(Node::Other { span: raw }), i + 1),
    }
}

/// `raw` less any leading `&anchor` and `!tag` and the space after each (T-160). Without this,
/// a use inside `x: &a "{{ y }}"` landed on `"{` instead of `y`.
fn past_properties(raw: Span, text: &str) -> Span {
    let b = text.as_bytes();
    let mut start = raw.start;
    while start < raw.end && matches!(b[start], b'&' | b'!') {
        while start < raw.end && !b[start].is_ascii_whitespace() {
            start += 1;
        }
        while start < raw.end && b[start].is_ascii_whitespace() {
            start += 1;
        }
    }
    Span { start, end: raw.end }
}

/// Where each content line of a `|` or `>` scalar starts, in its value and in the document.
///
/// Matched line by line against the value rather than re-deriving YAML's folding and
/// chomping rules: each content line, indentation stripped, appears verbatim in the value,
/// and only line-break separators (`\n`, or the space `>` folds one into) lie between them.
/// The indentation is the first content line's, less the spaces the value itself keeps
/// there, which covers an explicit indentation indicator without knowing the parent's.
/// `None` if anything fails to line up, so a reader falls back rather than guesses.
fn block_lines(value: &str, raw: Span, text: &str) -> Option<Box<[(usize, usize)]>> {
    let src = raw.slice(text);
    let header = src.find('\n')? + 1;
    let mut lines = Vec::new();
    let mut at = raw.start + header;
    for l in src[header..].split_inclusive('\n') {
        lines.push((at, l.trim_end_matches(['\n', '\r'])));
        at += l.len();
    }
    let lead = |s: &str| s.len() - s.trim_start_matches(' ').len();
    let (_, first) = lines.iter().find(|(_, b)| !b.trim().is_empty())?;
    let indent = lead(first).checked_sub(lead(value.trim_start_matches('\n')))?;

    let mut out = Vec::new();
    let mut cursor = 0;
    for (start, body) in lines {
        if body.trim().is_empty() {
            continue;
        }
        if lead(body) < indent {
            break;
        }
        let content = &body[indent..];
        while !value[cursor..].starts_with(content) {
            match value.as_bytes().get(cursor) {
                Some(b'\n' | b' ') => cursor += 1,
                _ => return None,
            }
        }
        out.push((cursor, start + indent));
        cursor += content.len();
    }
    Some(out.into_boxed_slice())
}

/// The same table for a flow scalar whose value is not its source slice: an escape in a
/// double-quoted one, `''` in a single-quoted one, or a line break folded in any of the three.
///
/// Decodes the source the way libyaml does, starting a new entry after each escape and each
/// fold, and keeps the table only if the decoding reproduces libyaml's value byte for byte. So
/// a rule decoded wrongly costs the table, never a wrong offset. `None` for the common case,
/// a value that is its own source.
fn flow_lines(value: &str, raw: Span, text: &str, style: ScalarStyle) -> Option<Box<[(usize, usize)]>> {
    let src = raw.slice(text);
    let quoted = matches!(style, ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted);
    let (body, base) = if quoted { (src.get(1..src.len().checked_sub(1)?)?, raw.start + 1) } else { (src, raw.start) };
    if body == value {
        return None;
    }
    let b = body.as_bytes();
    let mut out = String::new();
    let mut runs: Vec<(usize, usize)> = Vec::new();
    // `fresh`: the next character starts an entry. `hard`: how much of `out` a fold may not
    // trim — an escaped space or tab is content, not trailing whitespace.
    let mut fresh = true;
    let mut hard = 0;
    let push = |out: &mut String, runs: &mut Vec<(usize, usize)>, fresh: &mut bool, s: &str, at: usize| {
        if *fresh {
            runs.push((out.len(), base + at));
            *fresh = false;
        }
        out.push_str(s);
    };
    let blank = |c: u8| c == b' ' || c == b'\t';
    let skip_break = |i: usize| if b[i] == b'\r' && b.get(i + 1) == Some(&b'\n') { i + 2 } else { i + 1 };
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if style == ScalarStyle::DoubleQuoted => {
                if matches!(b.get(i + 1), Some(b'\r' | b'\n')) {
                    // An escaped line break: the break and the next line's indentation go.
                    i = skip_break(i + 1);
                    while i < b.len() && blank(b[i]) {
                        i += 1;
                    }
                    fresh = true;
                    continue;
                }
                let (s, len) = escape(&body[i + 1..])?;
                fresh = true;
                push(&mut out, &mut runs, &mut fresh, &s, i);
                fresh = true;
                hard = out.len();
                i += 1 + len;
            }
            b'\'' if style == ScalarStyle::SingleQuoted && b.get(i + 1) == Some(&b'\'') => {
                fresh = true;
                push(&mut out, &mut runs, &mut fresh, "'", i);
                fresh = true;
                i += 2;
            }
            b'\r' | b'\n' => {
                // Trailing whitespace before a break is dropped. One break folds to a space;
                // a break followed by empty lines becomes one newline per empty line.
                let keep = out.trim_end_matches([' ', '\t']).len().max(hard);
                out.truncate(keep);
                runs.retain(|&(v, _)| v < keep);
                let at = i;
                let mut empty = 0;
                i = skip_break(i);
                loop {
                    while i < b.len() && blank(b[i]) {
                        i += 1;
                    }
                    if i < b.len() && matches!(b[i], b'\r' | b'\n') {
                        empty += 1;
                        i = skip_break(i);
                    } else {
                        break;
                    }
                }
                fresh = true;
                let sep = if empty == 0 { " ".to_string() } else { "\n".repeat(empty) };
                push(&mut out, &mut runs, &mut fresh, &sep, at);
                fresh = true;
            }
            _ => {
                let ch = body[i..].chars().next()?;
                push(&mut out, &mut runs, &mut fresh, &body[i..i + ch.len_utf8()], i);
                i += ch.len_utf8();
            }
        }
    }
    (out == value).then(|| runs.into_boxed_slice())
}

/// One double-quoted escape, `s` starting just after its backslash: the text it stands for
/// and how many bytes of `s` it took. The set libyaml's scanner accepts.
fn escape(s: &str) -> Option<(String, usize)> {
    let c = *s.as_bytes().first()?;
    let simple = match c {
        b'0' => '\0',
        b'a' => '\x07',
        b'b' => '\x08',
        b't' | b'\t' => '\t',
        b'n' => '\n',
        b'v' => '\x0b',
        b'f' => '\x0c',
        b'r' => '\r',
        b'e' => '\x1b',
        b' ' => ' ',
        b'"' => '"',
        b'/' => '/',
        b'\'' => '\'',
        b'\\' => '\\',
        b'N' => '\u{85}',
        b'_' => '\u{a0}',
        b'L' => '\u{2028}',
        b'P' => '\u{2029}',
        b'x' | b'u' | b'U' => {
            let n = match c { b'x' => 2, b'u' => 4, _ => 8 };
            let code = u32::from_str_radix(s.get(1..1 + n)?, 16).ok()?;
            return Some((char::from_u32(code)?.to_string(), 1 + n));
        }
        _ => return None,
    };
    Some((simple.to_string(), 1))
}

/// A quoted scalar's marks span the quotes; `parse.rs` ranges the value, so match it.
fn unquote(mut span: Span, text: &str) -> Span {
    let s = span.slice(text);
    if s.len() >= 2 {
        let b = s.as_bytes();
        if (b[0] == b'"' || b[0] == b'\'') && b[0] == b[s.len() - 1] {
            span.start += 1;
            span.end -= 1;
        }
    }
    span
}

#[cfg(test)]
mod tests {
    use super::*;

    /// For each needle, where [`Node::doc_offset`] puts its start in the value of the scalar
    /// `pick` chooses, as the document text from there to the end of that line.
    fn landed(src: &str, pick: fn(&Node) -> &Node, needles: &[&str]) -> Vec<String> {
        let nodes = parse_lenient(src).expect("valid yaml");
        let c = pick(&nodes[0]);
        let value = c.as_str().expect("a scalar");
        needles
            .iter()
            .map(|n| {
                let o = value.find(n).unwrap_or_else(|| panic!("{n:?} in {value:?}"));
                let rest = &src[c.doc_offset(o)..];
                rest[..rest.find(['\r', '\n']).unwrap_or(rest.len())].to_string()
            })
            .collect()
    }

    fn key_c(n: &Node) -> &Node {
        n.get("c").expect("a c: key")
    }

    /// T-226. A block scalar's value is its source with each line's indentation stripped, so
    /// `span.start + offset` falls short by the indentation of every line before the use.
    /// Every style, a use on the first content line and on a later one.
    #[test]
    fn an_offset_into_a_block_scalar_lands_on_its_source() {
        let both = ["{{ x }}", "{{ y }}"];
        for src in [
            "c: |\n  {{ x }}\n  b {{ y }}\n",
            "c: |-\n  {{ x }}\n  b {{ y }}\n",
            "c: |+\n  {{ x }}\n  b {{ y }}\n\n\n",
            "c: >\n  {{ x }}\n  b {{ y }}\n",
            "c: >-\n  {{ x }}\n\n  b {{ y }}\n",
            // Folded, with a more-indented line: its break is kept, not folded to a space.
            "c: >\n  {{ x }}\n    more\n  b {{ y }}\n",
            // An explicit indicator, so the value's first line keeps three of its spaces.
            "c: |2\n     {{ x }}\n   b {{ y }}\n",
            "c: |  # a comment on the header\n  {{ x }}\n  b {{ y }}\n",
            // Leading blank lines are in the value as newlines.
            "c: |\n\n  {{ x }}\n  b {{ y }}\n",
            "c: |\r\n  {{ x }}\r\n  b {{ y }}\r\n",
        ] {
            assert_eq!(landed(src, key_c, &both), both, "{src:?}");
        }
        // Ten spaces in, under a list item — the reference tree's shape.
        let deep = "c:\n  - k: |\n          deep\n          {{ x }}\n          b {{ y }}\n";
        fn k(n: &Node) -> &Node {
            n.get("c").unwrap().items()[0].get("k").unwrap()
        }
        assert_eq!(landed(deep, k, &both), both);
    }

    /// A flow scalar whose value is not its source: escapes, `''`, and line folding. A use
    /// after each one, since every one shifts what follows it. Each value was measured to
    /// render its `{{ v }}` on ansible-core 2.21.2.
    #[test]
    fn an_offset_past_an_escape_or_a_fold_lands_on_its_source() {
        let x = ["{{ x }}"];
        for src in [
            "c: \"a\\tb\\n{{ x }}\"\n",
            "c: \"\\u00e9\\x41\\U0001F600 {{ x }}\"\n",
            "c: \"q\\\"q\\\\ {{ x }}\"\n",
            "c: \"\\  {{ x }}\"\n",
            "c: 'it''s {{ x }}'\n",
            "c: wrapped plain\n  line {{ x }}\n",
            "c: wrapped plain   \n  line\n\n\n  after blanks {{ x }}\n",
            "c: \"quoted\n    {{ x }}\"\n",
            "c: 'single\n\n    {{ x }}'\n",
            "c: \"escaped \\\n    break {{ x }}\"\n",
            "c: \"crlf\r\n    {{ x }}\"\r\n",
        ] {
            assert_eq!(landed(src, key_c, &x)[0].trim_end_matches(['"', '\'']), x[0], "{src:?}");
            assert!(matches!(key_c(&parse_lenient(src).unwrap()[0]), Node::Scalar { lines: Some(_), .. }), "{src:?}");
        }
        // Two escapes before two uses, so a table that only knew the first shift fails here.
        let two = "c: \"\\t{{ x }}\\t\\t{{ y }}\"\n";
        assert_eq!(landed(two, key_c, &["{{ x }}", "{{ y }}"]), ["{{ x }}\\t\\t{{ y }}\"", "{{ y }}\""]);
    }

    /// The table is kept only when decoding the source reproduces libyaml's value. A value the
    /// decoder disagrees with gets no table, and its readers fall back rather than guess.
    #[test]
    fn a_decoding_that_does_not_reproduce_the_value_is_dropped() {
        let src = "c: \"a\\tb {{ x }}\"\n";
        let raw = Span { start: 3, end: src.len() - 1 };
        assert!(flow_lines("a\tb {{ x }}", raw, src, ScalarStyle::DoubleQuoted).is_some(), "the control");
        assert!(flow_lines("a b {{ x }}", raw, src, ScalarStyle::DoubleQuoted).is_none());
    }

    /// The control: a flow scalar is a contiguous slice of the document, carries no line table,
    /// and has always been exact.
    #[test]
    fn an_offset_into_a_flow_scalar_is_still_span_start_plus_offset() {
        for src in ["c: a {{ x }}\n", "c: \"a {{ x }}\"\n", "c: 'a {{ x }}'\n"] {
            let nodes = parse_lenient(src).expect("valid yaml");
            assert!(matches!(nodes[0].get("c"), Some(Node::Scalar { lines: None, .. })), "{src:?}");
            let got = landed(src, key_c, &["{{ x }}"]);
            assert_eq!(got[0].trim_end_matches(['"', '\'']), "{{ x }}", "{src:?}");
        }
    }

    /// T-117. `when:` is YAML null and `when: ""` is an empty string; Ansible treats the first
    /// as absence and the second as fatal. libyaml hands both to us as an empty value, so the
    /// marks are the only thing that tells them apart — a null spans nothing, `""` spans its
    /// quotes. Getting this wrong warns on a task that runs perfectly well.
    #[test]
    fn a_plain_empty_scalar_is_null_not_an_empty_string() {
        let when = |src: &str| {
            let doc = crate::parse::Document::new(src.to_string());
            let nodes = doc.parse().expect("valid yaml");
            nodes[0].items()[0].get("when").cloned().expect("a when: key")
        };
        // The variant, not just `as_str()` — `Other` also has no string, so asserting only
        // the absence of one would pass with null folded back into it. It must not be: `Other`
        // means an alias or a shape we do not model, and null is neither.
        assert!(matches!(when("- when:\n  debug: x\n"), Node::Null { .. }));
        assert!(matches!(when("- when: ~\n  debug: x\n"), Node::Scalar { .. }), "`~` is written");

        let empty = when("- when: \"\"\n  debug: x\n");
        assert!(matches!(empty, Node::Scalar { .. }), "an explicit empty string is a value");
        assert_eq!(empty.as_str(), Some(""));
        assert_eq!(when("- when: '  '\n  debug: x\n").as_str(), Some("  "));

        // The span of a null sits where the value would have been, so a diagnostic anchored
        // on it still lands on the right line rather than at the top of the file.
        let src = "- when:\n  debug: x\n";
        let Node::Null { span } = when(src) else { unreachable!() };
        assert_eq!(span.start, span.end);
        assert_eq!(&src[..span.start], "- when:", "positioned after its key");
    }

    /// The three placements the demo fixture covers, in one file. Live-verified on
    /// ansible-core 2.21.2: four warnings, each anchored at the later occurrence.
    #[test]
    fn finds_duplicates_at_every_level() {
        let src = "- hosts: webservers\n  hosts: localhost\n  vars:\n    p: 80\n    p: 8080\n  \
                   tasks:\n    - when: false\n      when: true\n";
        let d = duplicate_keys(src);
        let names: Vec<&str> = d.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(names, ["hosts", "p", "when"]);
        // Anchored on the winner, and naming the loser.
        assert_eq!(d[1].span.slice(src), "p");
        assert_eq!(&src[d[1].span.start..d[1].span.start + 7], "p: 8080");
        assert_eq!(&src[d[1].first.start..d[1].first.start + 5], "p: 80");
    }

    /// Per mapping, not per file — otherwise every playbook with two tasks would light up.
    #[test]
    fn the_same_key_in_sibling_mappings_is_not_a_duplicate() {
        let src = "- name: one\n  debug: a\n- name: two\n  debug: b\n";
        assert_eq!(duplicate_keys(src), []);
        // Nesting is its own scope too: an inner `name:` does not collide with the outer.
        let src = "- name: outer\n  include_role:\n    name: inner\n";
        assert_eq!(duplicate_keys(src), []);
    }

    /// One entry per *later* occurrence, so three writes give two — matching Ansible, which
    /// warns twice. Each names the original as the discarded one.
    #[test]
    fn a_key_written_three_times_reports_twice() {
        let src = "a: 1\na: 2\na: 3\n";
        let d = duplicate_keys(src);
        assert_eq!(d.len(), 2);
        assert!(d.iter().all(|d| d.first.start == 0), "both point back at the first");
        assert!(d[0].span.start < d[1].span.start);
    }

    /// Flow style is the same mapping, and JSON is flow style — this is the file Ansible
    /// stays silent about, which is why the rule reports it separately (T-102).
    #[test]
    fn flow_and_json_mappings_are_scanned_too() {
        assert_eq!(duplicate_keys("{a: 1, a: 2}\n").len(), 1);
        assert_eq!(duplicate_keys("[{\"p\": 80, \"p\": 8080}]\n").len(), 1);
    }

    /// A file libyaml can't scan has no mappings to speak of; the unparseable hint owns it.
    #[test]
    fn unparseable_input_yields_nothing_rather_than_panicking() {
        assert_eq!(duplicate_keys("- name: Block form with a file: parameter\n"), []);
    }

    /// T-142. A block scalar whose last line has no trailing newline used to **panic** —
    /// `scan_block_scalar` ends its loop at end of input, then calls `read_line_break`
    /// anyway. libyaml's `READ_LINE` is a no-op there; the safe port turned that into
    /// `panic!`, so the process aborted on documents PyYAML and Ansible both accept. Found
    /// in `geerlingguy/ansible-for-devops`, `provisioning/tasks/composer.yml`.
    ///
    /// An editor re-parses on every keystroke and a buffer often has no final newline, so
    /// this was reachable constantly, not in some corner. Fixed in our `libyaml-safer` fork
    /// (see `[patch.crates.io]`); this pins it from our side.
    #[test]
    fn a_block_scalar_at_end_of_input_parses_instead_of_aborting() {
        let cmd = |src: &str| {
            let nodes = parse_lenient(src)
                .unwrap_or_else(|| panic!("must parse, not abort: {src:?}"));
            nodes[0].get("cmd").and_then(|v| v.as_str()).map(str::to_owned)
        };
        // Values cross-checked against PyYAML on the same input. The trailing newline is
        // *not* cosmetic — clip chomping keeps one only when the source has one — so the
        // pairs below must differ by exactly that. Equal values would mean we had padded
        // the input rather than fixed the scan.
        assert_eq!(cmd("cmd: >\n  mv a b\n  creates=c\n").as_deref(), Some("mv a b creates=c\n"));
        assert_eq!(cmd("cmd: >\n  mv a b\n  creates=c").as_deref(), Some("mv a b creates=c"));
        assert_eq!(cmd("cmd: |\n  mv a b\n  creates=c\n").as_deref(), Some("mv a b\ncreates=c\n"));
        assert_eq!(cmd("cmd: |\n  mv a b\n  creates=c").as_deref(), Some("mv a b\ncreates=c"));
    }

    #[test]
    fn accepts_the_underindented_scalar_class() {
        // The _run.yml class: a folded double-quoted scalar whose closing line sits at the
        // key column. Strict YAML 1.2 rejects it; Ansible (libyaml) accepts it.
        let src = "vars:\n  s: \"{{\n  (a | default(['{}']))\n  }}\"\n";
        let docs = parse_lenient(src).expect("libyaml should accept");
        let s = docs[0].get("vars").unwrap().get("s").unwrap();
        assert!(s.as_str().unwrap().contains("default"));
        assert!(parse_lenient_error(src).is_none(), "valid-to-Ansible → no error");
    }

    #[test]
    fn reports_error_only_when_broken_for_ansible_too() {
        // An unquoted `: ` in a value is invalid even to libyaml/PyYAML.
        let src = "---\n- name: Block form with a file: parameter\n";
        assert!(parse_lenient(src).is_none());
        let span = parse_lenient_error(src).expect("genuinely broken → a position");
        assert!(span.end > span.start);
    }

    #[test]
    fn spans_are_byte_accurate_past_non_ascii() {
        // Same fixture parse.rs pins: an em dash before the target shifts char markers off
        // the byte offset. The value must still slice out exactly.
        let src = "---\n\
                   - name: \"title with — an em dash\"\n  \
                   ansible.builtin.include_role: { name: cib-batch, tasks_from: begin }\n";
        let docs = parse_lenient(src).expect("valid yaml");
        let task = &docs[0].items()[0];
        let inc = task.get("ansible.builtin.include_role").expect("include_role");
        let name = inc.get("name").expect("name");
        let from = inc.get("tasks_from").expect("tasks_from");
        assert_eq!(name.span().slice(src), "cib-batch");
        assert_eq!(from.span().slice(src), "begin");
    }

    #[test]
    fn emoji_span_slices_exactly() {
        let src = "- name: \"🚀 launch\"\n  include_tasks: go.yml\n";
        let docs = parse_lenient(src).expect("valid yaml");
        let target = docs[0].items()[0].get("include_tasks").unwrap();
        assert_eq!(target.span().slice(src), "go.yml");
    }

    /// A failed parse always says where. Both functions run the same libyaml scan, so "no
    /// nodes" and "an error span" are one fact read twice — but callers treat them as two, and
    /// `bin/scan` carries a branch for the pair disagreeing. Pin the invariant so that branch
    /// stays the dead code it currently is, rather than becoming a file reported with no
    /// location. `corpus_smoke` asserts the same thing over every file in a real tree.
    #[test]
    fn a_file_that_does_not_parse_always_reports_where() {
        let cases = [
            "",
            "---\n",
            "- hosts: all\n",
            "{a: 1}\n",
            "---\n- one\n---\n- two\n",
            "- name: Block form with a file: parameter\n",
            "- a: [1, 2\n",
            "- a: \"unclosed\n",
            "key: value\n\tkey2: tab\n",
            "- *never_anchored\n",
            "%YAML 1.9\n---\n- x\n",
            "a: 1\n a: 2\n",
            "- !!binary not base64 !\n",
        ];
        for c in cases {
            assert_eq!(
                parse_lenient(c).is_none(),
                parse_lenient_error(c).is_some(),
                "the two disagree on {c:?}"
            );
        }
        // Measured at 6 of the 13. The guard is here because an invariant over inputs that all
        // parse is satisfied by two functions that both always return "fine".
        assert!(
            cases.iter().filter(|c| parse_lenient(c).is_none()).count() >= 4,
            "these cases must include real failures, or the invariant is untested"
        );
    }

    /// T-160: an alias is the anchored value, as Ansible's loader makes it. A collection takes
    /// the alias site's span and keeps its children's; a scalar keeps the anchor's span.
    #[test]
    fn an_alias_is_a_copy_of_its_anchor_placed_at_the_alias() {
        let src = "a: &m {k: 1}\nb: *m\nc: &s text\nd: *s\ne: &l [x]\nf: *l\n";
        let docs = parse_lenient(src).unwrap();
        let get = |key: &str| docs[0].entries().iter().find(|(k, _)| k.as_str() == Some(key)).unwrap().1.clone();
        let b = get("b");
        assert_eq!(b.span().slice(src), "*m", "a mapping lands at the alias");
        assert_eq!(b.entries()[0].1.as_str(), Some("1"));
        assert_eq!(b.entries()[0].0.span().slice(src), "k", "its children keep their own text");
        assert_eq!(get("d").as_str(), Some("text"));
        assert_eq!(get("d").span().slice(src), "text", "a scalar keeps the anchor's span, less `&s`");
        assert_eq!(get("c").span().slice(src), "text", "the anchored scalar itself too");
        assert!(matches!(get("f"), Node::Sequence { .. }));
        assert_eq!(get("f").span().slice(src), "*l");
        assert_eq!(
            aliases(src).iter().map(|(at, def)| (at.slice(src), def.slice(src))).collect::<Vec<_>>(),
            vec![("*m", "&m {k: 1}"), ("*s", "text"), ("*l", "&l [x]")],
            "the side table maps each alias site to its anchor; a collection's span starts at its `&`"
        );
    }

    /// A use inside an anchored or tagged scalar lands on the name, not on the properties.
    #[test]
    fn a_use_inside_an_anchored_scalar_lands_on_its_name() {
        for src in [
            "- hosts: all\n  vars:\n    x: &a \"{{ yy }}\"\n  tasks: []\n",
            "- hosts: all\n  vars:\n    x: !!str &a \"{{ yy }}\"\n  tasks: []\n",
            "- hosts: all\n  vars:\n    x: &a\n      \"{{ yy }}\"\n  tasks: []\n",
            "- hosts: all\n  vars:\n    x: &a {{ yy }}x\n  tasks: []\n",
        ] {
            let Some(nodes) = crate::parse::Document::new(src.into()).parse() else { continue };
            let u: Vec<&str> = crate::vars::any_uses(&nodes).into_iter().map(|u| u.span.slice(src)).collect();
            assert_eq!(u, ["yy"], "{src:?}");
        }
    }

    /// A merge key's aliased value stays opaque, so a merged task list is not read as a `<<`
    /// key holding a list; an inline merge value is left as written.
    #[test]
    fn a_merge_key_through_an_alias_stays_opaque() {
        let src = "l: &l [{a: 1}]\nm: &m {b: 2}\nx:\n  <<: *l\ny:\n  <<: *m\nz:\n  <<: [*m]\nw:\n  <<: {c: 3}\n";
        let docs = parse_lenient(src).unwrap();
        let merge = |key: &str| docs[0].entries().iter().find(|(k, _)| k.as_str() == Some(key)).unwrap().1.entries()[0].1.clone();
        for key in ["x", "y", "z"] {
            assert!(matches!(merge(key), Node::Other { .. }), "{key}");
        }
        assert!(matches!(merge("w"), Node::Mapping { .. }), "inline, as before");
    }

    /// T-102 walks the raw events, not the substituted tree, so a duplicate key inside an
    /// anchored mapping is one authoring mistake reported once, at the definition, however
    /// many aliases repeat it.
    #[test]
    fn a_duplicate_key_in_an_anchored_mapping_is_reported_once_at_its_definition() {
        let src = "a: &m {k: 1, k: 2}\nb: *m\nc: *m\n";
        let dupes = duplicate_keys(src);
        assert_eq!(dupes.len(), 1, "{dupes:?}");
        assert!(dupes[0].span.start < src.find("b:").unwrap(), "at the definition");
    }

    /// A self-reference loads upstream (measured, 2.21.3) and must not recurse here.
    #[test]
    fn a_recursive_anchor_terminates() {
        let src = "r: &a [*a]\n";
        let docs = parse_lenient(src).expect("loads, as upstream");
        assert!(matches!(&docs[0].entries()[0].1, Node::Sequence { items, .. } if matches!(items[..], [Node::Other { .. }])));
        assert_eq!(parse_lenient_error(src), None);
    }

    /// Measured on 2.21.3: "YAML parsing failed: Found undefined alias". libyaml scans it, so
    /// the check is ours, and both entry points agree on it.
    #[test]
    fn an_alias_to_an_undefined_anchor_is_a_parse_failure() {
        for src in ["r: *never\n", "a: *later\nb: &later 1\n", "---\nx: &d 1\n---\ny: *d\n"] {
            assert!(parse_lenient(src).is_none(), "{src:?}");
            let at = parse_lenient_error(src).expect("located");
            assert!(at.slice(src).starts_with('*'), "{src:?}: {:?}", at.slice(src));
        }
        assert!(parse_lenient("a: &x 1\nb: *x\n").is_some(), "the control");
    }

    #[test]
    #[ignore = "corpus smoke: ANSIBLE_CORPUS=<path> cargo test -p ansible-core corpus_smoke -- --ignored --nocapture"]
    fn corpus_smoke() {
        let Ok(root) = std::env::var("ANSIBLE_CORPUS") else { return };
        let root = std::path::PathBuf::from(root);
        if !root.is_dir() {
            return;
        }
        let files = crate::workspace::yaml_files(&root);
        let mut ok = 0;
        let mut bad = Vec::new();
        for f in &files {
            let Ok(text) = std::fs::read_to_string(f) else { continue };
            let parsed = parse_lenient(&text);
            assert_eq!(
                parsed.is_none(),
                parse_lenient_error(&text).is_some(),
                "{} parses and errors inconsistently",
                f.display()
            );
            if parsed.is_some() {
                ok += 1;
            } else {
                bad.push(f.strip_prefix(&root).unwrap_or(f).display().to_string());
            }
        }
        println!("files={} parsed={ok} rejected={}", files.len(), bad.len());
        for b in &bad {
            println!("  rejected: {b}");
        }
    }
}
