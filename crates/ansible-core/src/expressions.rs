//! Which strings in a file Ansible evaluates as bare Jinja expressions.
//!
//! `when:` is one of five such keywords and the rules in [`crate::condition`] only ever saw
//! that one, so the same fatal expression was diagnosed in one place and ignored in four
//! (T-141). Upstream groups them explicitly — `config/base.yml:78-93` describes the embedded
//! template cases as applying to "conditionals (for example, ``failed_when``, ``until``,
//! ``assert.that``)".
//!
//! This module answers *which* strings reach the rules, not how they are analysed. It walks
//! the raw tree rather than [`crate::ast`] because a `Directive` keeps only spans, and the
//! rules need the clause text; the walk covers plays, blocks and tasks alike, which is how
//! block-level conditions get diagnosed at all.

use crate::ast::{self, Ast};
use crate::parse::{Node, Span};

/// Keywords whose value is evaluated as a Jinja expression with no `{{ }}` around it. All
/// five are equally fatal when the expression is broken — verified on ansible-core 2.21.2,
/// where `failed_when: x = 'y'` dies with the same "chunk after expression" as `when:`.
///
/// `that` is not here because it is not a directive: it is a parameter of the `assert`
/// module, and is picked up separately.
pub const BARE_EXPRESSION: &[&str] = &["when", "failed_when", "changed_when", "until"];

/// One written occurrence of a bare-expression keyword.
#[derive(Debug, Clone)]
pub struct Site {
    /// As written, for a diagnostic that names the keyword it fired on. `assert:`'s
    /// parameter is reported as `that`.
    pub keyword: String,
    /// Span of the keyword itself — what an explanation of the guard anchors on.
    pub key_span: Span,
    /// Span of the value, where a problem with the expression is reported.
    pub value_span: Span,
    /// The expressions. A list value is several ANDed clauses, exactly as `when:` has always
    /// been; a scalar is one clause.
    pub clauses: Vec<String>,
    /// The owning mapping binds `item` — it has a `loop:`/`with_*` *and* has not renamed the
    /// loop variable. `loop_control: loop_var: thing` leaves `item` undefined, and a
    /// condition using it then dies exactly as it would with no loop at all (verified on
    /// ansible-core 2.21.2).
    pub binds_item: bool,
}

/// Every bare-expression site in a parsed file, in document order.
///
/// Returns nothing for a file that is neither a playbook nor a task list. A vars file is
/// free to have a key called `when`, and it would be data — the rules must not read it as a
/// condition.
pub fn sites(nodes: &[Node]) -> Vec<Site> {
    if matches!(ast::build(nodes), Ast::Other) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for n in nodes {
        walk(n, &mut out);
    }
    out
}

fn walk(node: &Node, out: &mut Vec<Site>) {
    match node {
        Node::Sequence { items, .. } => items.iter().for_each(|i| walk(i, out)),
        Node::Mapping { entries, .. } => {
            let binds_item = binds_item(node);
            for (k, v) in entries {
                let Some(key) = k.as_str() else { continue };
                if BARE_EXPRESSION.contains(&key) {
                    push(key, k.span(), v, binds_item, out);
                } else if is_assert(key) {
                    // `that:` is a module parameter, so it is one level down.
                    if let Some(Node::Mapping { entries, .. }) = Some(v) {
                        for (tk, tv) in entries {
                            if tk.as_str() == Some("that") {
                                push("that", tk.span(), tv, binds_item, out);
                            }
                        }
                    }
                }
                // A value bound under `vars:` is data even when it is named like a keyword,
                // and `set_fact:` writes variables rather than conditions.
                if !matches!(key, "vars" | "set_fact") {
                    walk(v, out);
                }
            }
        }
        _ => {}
    }
}

fn is_assert(key: &str) -> bool {
    crate::keywords::core_action(key) == "assert"
}

/// Does this task's loop define `item`? A loop binds one variable, named `item` by default
/// and renamed by `loop_control: loop_var:`. After a rename `item` is undefined again, so a
/// condition using it fails exactly as it would with no loop — treating any loop as "item is
/// fine" turns a fatal task into a silent one.
fn binds_item(node: &Node) -> bool {
    let looped = node.entries().iter().any(|(k, _)| {
        matches!(k.as_str(), Some(s) if s == "loop" || s.starts_with("with_"))
    });
    let renamed = node
        .get("loop_control")
        .and_then(|c| c.get("loop_var"))
        .and_then(Node::as_str)
        .is_some_and(|v| v != "item");
    looped && !renamed
}

/// A scalar value is one clause, a list is several ANDed — the shape `when:` already had.
/// A null value produces no clause and no site: `when:` with nothing after it is absence,
/// not an empty condition (T-117).
fn push(keyword: &str, key_span: Span, value: &Node, binds_item: bool, out: &mut Vec<Site>) {
    let clauses: Vec<String> = match value {
        Node::Sequence { items, .. } => {
            items.iter().filter_map(|i| i.as_str().map(str::to_owned)).collect()
        }
        other => other.as_str().map(str::to_owned).into_iter().collect(),
    };
    if clauses.is_empty() {
        return;
    }
    out.push(Site {
        keyword: keyword.to_string(),
        key_span,
        value_span: value.span(),
        clauses,
        binds_item,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    fn sites_of(src: &str) -> Vec<Site> {
        let doc = Document::new(src.to_string());
        sites(&doc.parse().expect("valid yaml"))
    }

    fn keywords(src: &str) -> Vec<String> {
        sites_of(src).into_iter().map(|s| s.keyword).collect()
    }

    #[test]
    fn all_five_bare_expression_keywords_are_found() {
        let src = "- command: echo hi\n  when: a\n  failed_when: b\n  changed_when: c\n  \
                   until: d\n- assert:\n    that: e\n";
        assert_eq!(keywords(src), ["when", "failed_when", "changed_when", "until", "that"]);
    }

    /// The clause shapes are `when:`'s: one scalar, or a list ANDed together.
    #[test]
    fn a_list_value_is_several_clauses() {
        let s = sites_of("- assert:\n    that:\n      - a == 1\n      - b == 2\n");
        assert_eq!(s[0].clauses, ["a == 1", "b == 2"]);
        let one = sites_of("- command: x\n  failed_when: a == 1\n");
        assert_eq!(one[0].clauses, ["a == 1"]);
    }

    /// The old reference-carried path could only see a task that produced a file reference.
    /// A block's `when:` was never diagnosed at all.
    #[test]
    fn a_block_level_when_is_a_site() {
        let src = "- block:\n    - command: x\n  when: risky\n";
        let s = sites_of(src);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].clauses, ["risky"]);
    }

    #[test]
    fn a_loop_on_the_owning_task_is_carried() {
        let s = sites_of("- command: x\n  loop: [1, 2]\n  failed_when: item > 1\n");
        assert!(s[0].binds_item);
        let s = sites_of("- command: x\n  with_items: [1]\n  when: item\n");
        assert!(s[0].binds_item);
        let s = sites_of("- command: x\n  failed_when: item > 1\n");
        assert!(!s[0].binds_item);
    }

    /// A loop binds *one* variable and `loop_control:` renames it. After a rename `item` is
    /// undefined again — live on 2.21.2, `loop_var: thing` with `when: item > 0` fails with
    /// "'item' is undefined". Reading any loop as "item is fine" hides a task that dies.
    #[test]
    fn a_renamed_loop_variable_does_not_bind_item() {
        let renamed = "- command: x\n  loop: [1, 2]\n  loop_control:\n    loop_var: thing\n  \
                       when: item > 0\n";
        assert!(!sites_of(renamed)[0].binds_item);

        // Spelling the default explicitly still binds it, and so does any other
        // `loop_control:` setting.
        let explicit = "- command: x\n  loop: [1]\n  loop_control:\n    loop_var: item\n  \
                        when: item > 0\n";
        assert!(sites_of(explicit)[0].binds_item);
        let other = "- command: x\n  loop: [1]\n  loop_control:\n    label: x\n  when: item\n";
        assert!(sites_of(other)[0].binds_item);
    }

    /// A null value is absence, not an empty condition — so it is not a site at all, and
    /// nothing downstream has to re-derive that distinction (T-117).
    #[test]
    fn a_null_value_is_not_a_site_but_an_empty_string_is() {
        assert!(sites_of("- command: x\n  when:\n").is_empty());
        assert_eq!(sites_of("- command: x\n  when: \"\"\n")[0].clauses, [""]);
    }

    /// A vars file is data. It may have a key called `when` and it means nothing.
    #[test]
    fn a_vars_file_has_no_sites() {
        assert!(sites_of("when: not a condition\nfailed_when: nor this\n").is_empty());
        // Nor does a `vars:` mapping inside a real task file.
        assert!(sites_of("- command: x\n  vars:\n    when: data\n").is_empty());
    }

    /// The module may be written short or fully qualified.
    #[test]
    fn assert_is_found_under_either_name() {
        assert_eq!(keywords("- ansible.builtin.assert:\n    that: x\n"), ["that"]);
        assert_eq!(keywords("- assert:\n    that: x\n"), ["that"]);
        // `fail_msg` is a message, not an expression.
        assert_eq!(keywords("- assert:\n    that: x\n    fail_msg: nope\n"), ["that"]);
    }

    /// Spans have to point at the value, or a diagnostic lands on the wrong line.
    #[test]
    fn spans_cover_the_keyword_and_its_value() {
        let src = "- command: x\n  failed_when: a = 1\n";
        let s = &sites_of(src)[0];
        assert_eq!(s.key_span.slice(src), "failed_when");
        assert_eq!(s.value_span.slice(src), "a = 1");
    }
}
