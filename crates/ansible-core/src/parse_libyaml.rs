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
            if parse_lenient(&text).is_some() {
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
