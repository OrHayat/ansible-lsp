//! T-105: `delegate_to:` values that name a host outright.
//!
//! Measured on 2.21.3: a name no inventory lists is not an error. `VariableManager` builds a
//! `Host` for it on the spot (`vars/manager.py:546-547`) and the task connects to whatever that
//! name resolves to — a typo surfaces as `UNREACHABLE … Could not resolve hostname`, never as an
//! unknown host. A literal `""` is no delegation at all (the task runs on the play's host), and
//! a template is not judged here.

use crate::ast::{Ast, PlayItem, Stmt};
use crate::parse::{node_with_span, Node, Span};

/// Every literal `delegate_to:` on a task or block, with the value's span.
pub fn literal_hosts(ast: &Ast, nodes: &[Node]) -> Vec<(String, Span)> {
    fn walk(s: &Stmt, nodes: &[Node], out: &mut Vec<(String, Span)>) {
        let ds = match s {
            Stmt::Block(b) => {
                b.block.iter().chain(&b.rescue).chain(&b.always).for_each(|c| walk(c, nodes, out));
                &b.directives
            }
            // `local_action` overrides it with `localhost` (`mod_args.py:325`): never connected to.
            Stmt::Task(t) if node_with_span(nodes, t.span).is_some_and(|n| n.get("local_action").is_some()) => return,
            Stmt::Task(t) => &t.directives,
        };
        // The last occurrence is the one Ansible keeps.
        let Some(d) = ds.iter().rev().find(|d| d.key == "delegate_to") else { return };
        let Some(v) = node_with_span(nodes, d.value).and_then(Node::as_str).map(str::trim) else { return };
        if !v.is_empty() && !v.contains("{{") && !v.contains("{%") {
            out.push((v.to_string(), d.value));
        }
    }
    let mut out = Vec::new();
    match ast {
        Ast::Playbook(items) => {
            for item in items {
                let PlayItem::Play(p) = item else { continue };
                for s in p.pre_tasks.iter().chain(&p.tasks).chain(&p.post_tasks).chain(&p.handlers) {
                    walk(s, nodes, &mut out);
                }
            }
        }
        Ast::Tasks(list) => list.iter().for_each(|s| walk(s, nodes, &mut out)),
        Ast::Other => {}
    }
    out.sort_by_key(|(_, s)| s.start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    fn hosts(src: &str) -> Vec<String> {
        let nodes = Document::new(src.into()).parse().unwrap();
        literal_hosts(&crate::ast::build(&nodes), &nodes).into_iter().map(|(h, _)| h).collect()
    }

    #[test]
    fn a_literal_on_a_task_a_block_and_a_handler_is_collected() {
        let src = "- hosts: all\n  tasks:\n    - command: x\n      delegate_to: wbe1\n    - block:\n        - command: y\n      delegate_to: db9\n  handlers:\n    - name: h\n      command: z\n      delegate_to: h1\n";
        assert_eq!(hosts(src), vec!["wbe1", "db9", "h1"]);
    }

    #[test]
    fn a_task_file_is_collected() {
        assert_eq!(hosts("- command: x\n  delegate_to: wbe1\n"), vec!["wbe1"]);
    }

    #[test]
    fn templated_and_empty_values_are_not() {
        for v in ["\"{{ groups.web[0] }}\"", "\"\"", "\"{% if a %}x{% endif %}\""] {
            assert_eq!(hosts(&format!("- command: x\n  delegate_to: {v}\n")), Vec::<String>::new(), "{v}");
        }
    }

    /// `local_action` sets `delegate_to = 'localhost'` over what was written (`mod_args.py:325`),
    /// measured in `demo/placement.yml` row 24 — the written host is never connected to.
    #[test]
    fn a_task_with_local_action_is_not_collected() {
        assert_eq!(hosts("- local_action: debug msg=y\n  delegate_to: other\n"), Vec::<String>::new());
        assert_eq!(hosts("- local_action:\n  delegate_to: other\n"), Vec::<String>::new());
        assert_eq!(hosts("- action: debug msg=y\n  delegate_to: other\n"), vec!["other"]);
    }

    #[test]
    fn the_span_is_the_value() {
        let src = "- command: x\n  delegate_to: wbe1\n";
        let nodes = Document::new(src.into()).parse().unwrap();
        let got = literal_hosts(&crate::ast::build(&nodes), &nodes);
        assert_eq!(got[0].1.slice(src), "wbe1");
    }
}
