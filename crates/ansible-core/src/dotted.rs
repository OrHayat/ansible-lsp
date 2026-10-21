//! T-221: `{{ db.port }}`, where `db` is a literal mapping we parsed. A variable use spans the
//! root only; this finds the `.key` chain after it and walks the root's written value.
//!
//! Measured on 2.21.3 with `db: {port: 5432, items: x, nested: {deep: 1}}` in `group_vars`:
//! `db.port` and `db.nested.deep` render the values, and `db.prot` fails with "object of type
//! 'dict' has no attribute". `db.items` renders the dict's **method**, not the `items:` key,
//! so a name the dict itself has never reaches a key.

use std::path::Path;

use crate::ast::{self, Ast};
use crate::cache::ScanCache;
use crate::condition;
use crate::parse::{node_with_span, Node, Span};
use crate::vars::{self, Located};

/// `# noqa` id for a key a literal mapping provably lacks.
pub const RULE_ID: &str = "undefined-key";

/// The public attributes of the dict a template sees: `_AnsibleLazyTemplateDict` has exactly
/// `dict`'s, read with `dir()` on 2.21.3.
pub const DICT_ATTRIBUTES: &[&str] =
    &["clear", "copy", "fromkeys", "get", "items", "keys", "pop", "popitem", "setdefault", "update", "values"];

/// The `.key` chain written right after a use ending at `use_end`, up to and including the
/// segment holding `byte`, with that segment's span. `None` when `byte` is on no segment, or
/// the chain breaks first — a subscript, a filter, a space.
pub fn chain_at(text: &str, use_end: usize, byte: usize) -> Option<(Vec<&str>, Span)> {
    let segs = chain(text, use_end);
    let i = segs.iter().position(|(_, s)| byte >= s.start && byte < s.end)?;
    Some((segs[..=i].iter().map(|(n, _)| *n).collect(), segs[i].1))
}

/// Every `.key` segment written right after a use ending at `use_end`, until the chain breaks.
pub fn chain(text: &str, use_end: usize) -> Vec<(&str, Span)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = use_end;
    while b.get(i) == Some(&b'.') {
        let start = i + 1;
        if !b.get(start).is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_') {
            break;
        }
        let end = start + b[start..].iter().take_while(|c| c.is_ascii_alphanumeric() || **c == b'_').count();
        out.push((&text[start..end], Span { start, end }));
        i = end;
    }
    out
}

/// A `.key` read that fails: the root's only definition is a literal mapping without that key.
#[derive(Debug, Clone)]
pub struct AbsentKey {
    /// The missing segment.
    pub span: Span,
    /// `db.prot`, through the missing segment.
    pub dotted: String,
    /// The mapping that lacks it: the root's definition, or the key above the missing one.
    pub mapping: String,
    pub def: Located,
}

/// Reads of a key a literal mapping does not have. Measured on 2.21.3: `db.prot` fails the
/// task with "object of type 'dict' has no attribute 'prot'", while `db.prot | default(1)`,
/// `db.prot is defined`, a ternary on `db.prot is defined`, and a task `when: db.prot is
/// defined` all run. So a read is reported only when nothing guards it, and only where
/// [`walk`]'s rules hold: one reachable definition (even a later one counts, as for
/// `var-undefined`), under no `when:`, written as a literal mapping all the way down. A `<<`
/// merge key or an alias may bring keys we cannot see, so either silences the mapping that
/// holds it. Playbooks only: a task file's caller can pass its own `db`.
pub fn absent_keys(path: &Path, nodes: &[Node], text: &str, cache: &ScanCache) -> Vec<AbsentKey> {
    if !matches!(ast::build(nodes), Ast::Playbook(_)) {
        return Vec::new();
    }
    let all = vars::definitions_with_deps_in(path, nodes, cache).0;
    let mut out = Vec::new();
    for u in vars::any_uses(nodes) {
        let segs = chain(text, u.span.end);
        if segs.is_empty() || guarded(&u, text) {
            continue;
        }
        let mut defs = all.iter().filter(|d| d.name == u.name && d.reaches(&u, path));
        let (Some(def), None) = (defs.next(), defs.next()) else { continue };
        // A task's or block's `vars:` reaches reads outside it in the index (T-249), so a read
        // elsewhere would be told "no key" when the name is undefined there.
        if def.condition.is_some() || matches!(def.source, vars::VarSource::TaskVars | vars::VarSource::BlockVars) {
            continue;
        }
        let source = (def.file != path).then(|| cache.source(&def.file)).flatten();
        let def_nodes = match &source {
            Some(s) => match &s.nodes {
                Some(n) => n.as_slice(),
                None => continue,
            },
            None if def.file == path => nodes,
            None => continue,
        };
        let Some(mut cur) = node_with_span(def_nodes, def.span) else { continue };
        let mut mapping = u.name.clone();
        for (i, (seg, span)) in segs.iter().enumerate() {
            let Node::Mapping { entries, .. } = cur else { break };
            if DICT_ATTRIBUTES.contains(seg) || entries.iter().any(|(k, _)| k.as_str() == Some("<<")) {
                break;
            }
            match entries.iter().rev().find(|(k, _)| k.as_str() == Some(seg)) {
                Some((_, v)) => {
                    cur = v;
                    mapping = format!("{mapping}.{seg}");
                }
                None => {
                    let names: Vec<&str> = segs[..=i].iter().map(|(n, _)| *n).collect();
                    out.push(AbsentKey {
                        span: *span,
                        dotted: format!("{}.{}", u.name, names.join(".")),
                        mapping,
                        def: def.clone(),
                    });
                    break;
                }
            }
        }
    }
    out
}

/// The read handles the root being undefined, so it handles a missing key too: a `default`
/// or `is defined` in its own expression, or a task `when:` testing the root. An expression
/// we cannot place counts as guarded, the silent direction.
fn guarded(u: &vars::VarUse, text: &str) -> bool {
    let Span { start, end } = u.expr;
    if start > u.span.start || end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return true;
    }
    condition::guard_at(&text[start..end], u.span.start - start).is_some()
        || u.guard.iter().any(|g| condition::positively_defined(g).contains(&u.name))
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

    /// One row per shape, from a real project: the three reads that fail, and every shape the
    /// rule must leave alone.
    #[test]
    fn a_missing_key_is_reported_only_where_the_read_fails_for_certain() {
        let root = crate::testing::project(
            "t221-absent",
            "[defaults]\n",
            &[("iv.yml", "iv: {a: 1}\n"), (
                "group_vars/all.yml",
                "base: &b {x: 1}\ndb:\n  port: 5432\n  nested: {deep: 1}\nmerged:\n  <<: *b\n  y: 2\naliased: *b\ntwice: {a: 1}\n",
            )],
        );
        let reads = [
            ("db.prot", true),
            ("db.nested.nope", true),
            ("db.port", false),
            ("db.nested.deep", false),
            ("db.prot | default(1)", false),
            ("db.prot | d(1)", false),
            ("db.prot is defined", false),
            ("db.prot if db.prot is defined else 2", false),
            ("db.items", false),
            ("db.keys", false),
            ("db['prot']", false),
            ("(db | default({})).prot", false),
            ("merged.x", false),
            ("aliased.x", false),
            ("twice.x", false),
            ("sf.x", false),
            ("rr.x", false),
            ("cv.x", false),
            ("nothing.x", false),
            ("tv.x", false),
            ("iv.x", false),
        ];
        let tasks: String = reads.iter().map(|(e, _)| format!("    - debug: {{msg: \"{{{{ {e} }}}}\"}}\n")).collect();
        let text = format!(
            "- hosts: all\n  vars:\n    twice: {{a: 2}}\n  tasks:\n    - set_fact: {{sf: {{a: 1}}}}\n    \
             - command: \"true\"\n      register: rr\n{tasks}    \
             - debug: {{msg: \"{{{{ cv.x }}}}\"}}\n      vars: {{cv: {{a: 1}}}}\n      when: y\n    \
             - debug: {{msg: \"{{{{ db.prot }}}}\"}}\n      when: db.prot is defined\n    \
             - debug: {{msg: hi}}\n      when: db.prot\n    \
             - debug: {{msg: hi}}\n      vars: {{tv: {{a: 1}}}}\n    \
             - include_vars: {{file: iv.yml}}\n      when: y\n"
        );
        let path = root.join("site.yml");
        let nodes = crate::parse::Document::new(text.clone()).parse().unwrap();
        let cache = ScanCache::new(crate::fs::StdFs);
        let got: Vec<String> = absent_keys(&path, &nodes, &text, &cache).into_iter().map(|a| a.dotted).collect();
        let mut want: Vec<String> = reads.iter().filter(|(_, f)| *f).map(|(e, _)| e.to_string()).collect();
        want.push("db.prot".into()); // the bare `when: db.prot`
        assert_eq!(got, want);

        // The role's own default reaches its tasks, and its caller may still pass another `rdb`.
        std::fs::create_dir_all(root.join("roles/r/defaults")).unwrap();
        std::fs::write(root.join("roles/r/defaults/main.yml"), "rdb: {port: 1}\n").unwrap();
        let task_file = root.join("roles/r/tasks/main.yml");
        let t = "- debug: {msg: \"{{ rdb.prot }}\"}\n";
        let tn = crate::parse::Document::new(t.into()).parse().unwrap();
        assert!(absent_keys(&task_file, &tn, t, &cache).is_empty(), "a caller can pass its own rdb");
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
