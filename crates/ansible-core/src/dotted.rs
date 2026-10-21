//! T-221: `{{ db.port }}`, where `db` is a literal mapping we parsed. A variable use spans the
//! root only; this finds the `.key` chain after it and walks the root's written value.
//!
//! Measured on 2.21.3 with `db: {port: 5432, items: x, nested: {deep: 1}}` in `group_vars`:
//! `db.port` and `db.nested.deep` render the values, and `db.prot` fails with "object of type
//! 'dict' has no attribute". `db.items` renders the dict's **method**, not the `items:` key,
//! so a name the dict itself has never reaches a key.

use crate::parse::{Node, Span};

/// The public attributes of the dict a template sees: `_AnsibleLazyTemplateDict` has exactly
/// `dict`'s, read with `dir()` on 2.21.3.
pub const DICT_ATTRIBUTES: &[&str] =
    &["clear", "copy", "fromkeys", "get", "items", "keys", "pop", "popitem", "setdefault", "update", "values"];

/// The `.key` chain written right after a use ending at `use_end`, up to and including the
/// segment holding `byte`, with that segment's span. `None` when `byte` is on no segment, or
/// the chain breaks first — a subscript, a filter, a space.
pub fn chain_at(text: &str, use_end: usize, byte: usize) -> Option<(Vec<&str>, Span)> {
    let b = text.as_bytes();
    let mut chain = Vec::new();
    let mut i = use_end;
    while byte >= i && b.get(i) == Some(&b'.') {
        let start = i + 1;
        if !b.get(start).is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_') {
            return None;
        }
        let end = start + b[start..].iter().take_while(|c| c.is_ascii_alphanumeric() || **c == b'_').count();
        chain.push(&text[start..end]);
        if byte >= start && byte < end {
            return Some((chain, Span { start, end }));
        }
        i = end;
    }
    None
}

/// The key and value `chain` reaches from `value`, through literal mappings only. On a
/// duplicate key the last one loads. `None` for anything else: a template, a scalar, a key
/// that is not written, or a name the dict has as an attribute.
pub fn walk<'a>(value: &'a Node, chain: &[&str]) -> Option<(&'a Node, &'a Node)> {
    let mut cur = value;
    let mut hit = None;
    for seg in chain {
        if DICT_ATTRIBUTES.contains(seg) {
            return None;
        }
        let Node::Mapping { entries, .. } = cur else { return None };
        let (k, v) = entries.iter().rev().find(|(k, _)| k.as_str() == Some(seg))?;
        hit = Some((k, v));
        cur = v;
    }
    hit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    #[test]
    fn the_chain_stops_at_the_segment_under_the_cursor() {
        let t = "{{ db.nested.deep }}";
        let end = 5; // after `db`
        let at = |word: &str| t.find(word).unwrap();
        assert_eq!(chain_at(t, end, at("nested")).map(|c| c.0), Some(vec!["nested"]));
        assert_eq!(chain_at(t, end, at("deep") + 2).map(|c| c.0), Some(vec!["nested", "deep"]));
        assert_eq!(chain_at(t, end, at("deep")).unwrap().1.slice(t), "deep");
        assert_eq!(chain_at(t, end, at("db")), None, "the root is not a segment");
        assert_eq!(chain_at(t, end, t.len() - 1), None, "past the chain");
        for broken in ["{{ db['port'] }}", "{{ db | x }}", "{{ db. port }}", "{{ db.0 }}"] {
            assert_eq!(chain_at(broken, 5, 8), None, "{broken}");
        }
    }

    #[test]
    fn only_written_keys_of_literal_mappings_are_reached() {
        let src = "db:\n  port: 5432\n  items: x\n  nested: {deep: 1}\n  t: \"{{ y }}\"\n  port: 6432\n";
        let nodes = Document::new(src.into()).parse().unwrap();
        let db = &nodes[0].entries()[0].1;
        let at = |chain: &[&str]| walk(db, chain).map(|(_, v)| v.as_str().unwrap_or("<map>").to_string());
        assert_eq!(at(&["port"]), Some("6432".into()), "the last duplicate loads");
        assert_eq!(at(&["nested", "deep"]), Some("1".into()));
        assert_eq!(at(&["items"]), None, "the dict's method, not the key");
        assert_eq!(at(&["prot"]), None);
        assert_eq!(at(&["t", "x"]), None, "a template is not walked");
        assert_eq!(at(&["port", "x"]), None, "a scalar has no keys");
    }
}
