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

use libyaml_safer::{Event, EventData, Parser};

use crate::parse::{Node, Span};

/// Parse `text` into one [`Node`] per document. `None` if libyaml itself can't scan it —
/// i.e. broken for Ansible too, not merely stricter-than-Ansible.
pub fn parse_lenient(text: &str) -> Option<Vec<Node>> {
    let events = events(text)?;

    let mut docs = Vec::new();
    let mut i = 0;
    while i < events.len() {
        match events[i].data {
            EventData::DocumentStart { .. } => {
                let (node, next) = build(&events, i + 1, text);
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

/// Build one node starting at event `i`; returns it and the index just past it.
fn build(events: &[Event], i: usize, text: &str) -> (Option<Node>, usize) {
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
        EventData::Scalar { value, .. } => {
            (Some(Node::Scalar { value: value.clone(), span: unquote(raw, text) }), i + 1)
        }
        EventData::SequenceStart { .. } => {
            let mut items = Vec::new();
            let mut j = i + 1;
            while !matches!(events[j].data, EventData::SequenceEnd) {
                let (item, next) = build(events, j, text);
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
                let (key, after_key) = build(events, j, text);
                let (val, after_val) = build(events, after_key, text);
                if let (Some(key), Some(val)) = (key, val) {
                    entries.push((key, val));
                }
                j = after_val;
            }
            let span = Span { start: raw.start, end: events[j].end_mark.index as usize };
            (Some(Node::Mapping { entries, span }), j + 1)
        }
        // Aliases and anything unexpected: a positioned Other, so spans stay sane.
        _ => (Some(Node::Other { span: raw }), i + 1),
    }
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
