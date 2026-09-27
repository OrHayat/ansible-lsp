//! T-109: keyword values outside their closed set. Every key here is legal where it sits —
//! what is wrong is the value, which neither [`crate::attributes`] nor [`crate::placement`]
//! reads. Measured on ansible-core 2.21.3.
//!
//! - `debugger:` — `--syntax-check` passes. The value is checked when a task it applies to
//!   runs, and that task fails with "Error processing keyword 'debugger'". A task skipped by
//!   `when:` never checks it; `ignore_errors: true` swallows the failure like any other.
//! - `order:` — `--syntax-check` passes; the play dies before its first task with "Invalid
//!   'order' specified for inventory hosts".
//! - `serial:` — never an error, but two spellings do the opposite of what they read as.
//!   `0` and below mean *no* batching (one batch of every host), and a percentage of `0%` or
//!   below means one host at a time, because a percentage never yields a batch under one.
//!
//! Both enums are case-sensitive and both keys template, so a `{{ }}` value is skipped.
//! The value sets live with the keyword tables ([`keywords::DEBUGGER_VALUES`],
//! [`keywords::ORDER_VALUES`]).

use crate::ast::{Ast, Directive, PlayItem, Stmt};
use crate::keywords;
use crate::parse::{Node, Span};

/// Rule id for the two values Ansible refuses.
pub const RULE_ID: &str = "invalid-keyword-value";

/// Ours, not ansible-core's: a `serial:` that runs, but not in the batches it reads as.
pub const SERIAL_RULE_ID: &str = "serial-batch-size";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Error,
    Hint,
}

#[derive(Debug, Clone)]
pub struct Problem {
    /// The offending value.
    pub span: Span,
    pub tier: Tier,
    pub message: String,
    pub rule: &'static str,
}

/// Where a directive sits, which decides what a bad `debugger:` takes down with it.
#[derive(Clone, Copy)]
enum Scope {
    Play,
    Block,
    Task,
}

pub fn problems(ast: &Ast, nodes: &[Node]) -> Vec<Problem> {
    let mut out = Vec::new();
    match ast {
        Ast::Playbook(items) => {
            for item in items {
                // `import_playbook` entries are left alone: `debugger:` is legal on one, but
                // what it does there is unmeasured.
                let PlayItem::Play(p) = item else { continue };
                directives(&p.directives, Scope::Play, nodes, &mut out);
                for s in p.pre_tasks.iter().chain(&p.tasks).chain(&p.post_tasks).chain(&p.handlers) {
                    stmt(s, nodes, &mut out);
                }
            }
        }
        Ast::Tasks(stmts) => {
            for s in stmts {
                stmt(s, nodes, &mut out);
            }
        }
        Ast::Other => {}
    }
    out
}

fn stmt(s: &Stmt, nodes: &[Node], out: &mut Vec<Problem>) {
    match s {
        Stmt::Block(b) => {
            directives(&b.directives, Scope::Block, nodes, out);
            for child in b.block.iter().chain(&b.rescue).chain(&b.always) {
                stmt(child, nodes, out);
            }
        }
        Stmt::Task(t) => directives(&t.directives, Scope::Task, nodes, out),
    }
}

fn directives(ds: &[Directive], scope: Scope, nodes: &[Node], out: &mut Vec<Problem>) {
    for key in ["debugger", "order", "serial"] {
        // On a duplicate key Ansible loads only the last, so only the last is judged.
        let Some(d) = ds.iter().rev().find(|d| d.key == key) else { continue };
        let Some(value) = node_at(nodes, d.value) else { continue };
        match key {
            "debugger" => enum_value(value, keywords::DEBUGGER_VALUES, |v| debugger_message(v, scope), out),
            "order" => enum_value(value, keywords::ORDER_VALUES, order_message, out),
            _ => serial(value, out),
        }
    }
}

fn enum_value(value: &Node, legal: &[&str], message: impl Fn(&str) -> String, out: &mut Vec<Problem>) {
    let Some(v) = literal(value) else { return };
    if !legal.contains(&v) {
        out.push(Problem { span: value.span(), tier: Tier::Error, message: message(v), rule: RULE_ID });
    }
}

fn debugger_message(v: &str, scope: Scope) -> String {
    let fails = match scope {
        Scope::Task => "this task fails whenever it runs",
        Scope::Block | Scope::Play => "every task it applies to fails whenever it runs",
    };
    format!(
        "`{v}` is not a valid `debugger` value — {fails}, though `--syntax-check` passes. \
         Must be one of: {}",
        keywords::DEBUGGER_VALUES.join(", ")
    )
}

fn order_message(v: &str) -> String {
    format!(
        "`{v}` is not a valid `order` value — the play fails before its first task, though \
         `--syntax-check` passes. Must be one of: {}",
        keywords::ORDER_VALUES.join(", ")
    )
}

fn serial(value: &Node, out: &mut Vec<Problem>) {
    match value {
        Node::Sequence { items, .. } => {
            for item in items {
                serial_item(item, true, out);
            }
        }
        _ => serial_item(value, false, out),
    }
}

fn serial_item(item: &Node, in_list: bool, out: &mut Vec<Problem>) {
    let Some(v) = literal(item) else { return };
    let message = if let Some(pct) = v.strip_suffix('%') {
        match pct.trim().parse::<i64>() {
            Ok(n) if n <= 0 => format!(
                "`{v}` is a batch of one host, not zero — a percentage never makes a batch \
                 smaller than one host"
            ),
            _ => return,
        }
    } else {
        match v.parse::<i64>() {
            Ok(n) if n <= 0 && in_list => {
                format!("a batch of `{v}` takes all remaining hosts at once, not none")
            }
            Ok(n) if n <= 0 => {
                format!("`serial: {v}` runs every host in one batch — 0 or below means no batching")
            }
            _ => return,
        }
    };
    out.push(Problem { span: item.span(), tier: Tier::Hint, message, rule: SERIAL_RULE_ID });
}

/// A scalar's text, unless it is templated — both keys render before they are checked.
fn literal(n: &Node) -> Option<&str> {
    let v = n.as_str()?.trim();
    (!v.contains("{{") && !v.contains("{%")).then_some(v)
}

/// The node whose span is exactly `span`. Directives carry only the value's span.
fn node_at(nodes: &[Node], span: Span) -> Option<&Node> {
    nodes.iter().find_map(|n| {
        let s = n.span();
        if s == span {
            return Some(n);
        }
        if s.start > span.start || s.end < span.end {
            return None;
        }
        match n {
            Node::Sequence { items, .. } => node_at(items, span),
            Node::Mapping { entries, .. } => {
                entries.iter().find_map(|(k, v)| node_at(std::slice::from_ref(k), span).or_else(|| node_at(std::slice::from_ref(v), span)))
            }
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    fn run(src: &str) -> Vec<(String, Tier)> {
        let doc = Document::new(src.into());
        let nodes = doc.parse().unwrap();
        problems(&crate::ast::build(&nodes), &nodes)
            .into_iter()
            .map(|p| (p.span.slice(src).to_string(), p.tier))
            .collect()
    }

    /// A role's `tasks/main.yml` is a task file, not a playbook — the task walk covers it.
    #[test]
    fn a_task_file_is_checked() {
        let got = run("- debug: msg=x\n  debugger: sometimes\n- debug: msg=x\n  debugger: never\n");
        assert_eq!(got, vec![("sometimes".into(), Tier::Error)]);
    }

    /// Ansible loads the last of a duplicated key; the first is never seen.
    #[test]
    fn only_the_last_duplicate_is_judged() {
        let src = "- hosts: all\n  order: random\n  order: sorted\n  tasks: []\n";
        assert_eq!(run(src), vec![]);
        let src = "- hosts: all\n  order: sorted\n  order: random\n  tasks: []\n";
        assert_eq!(run(src), vec![("random".into(), Tier::Error)]);
    }

    /// `serial` in a task is an invalid attribute, reported by that rule; it must not also
    /// get a batching hint about a batch that does not exist.
    #[test]
    fn serial_off_a_play_is_not_judged() {
        assert_eq!(run("- debug: msg=x\n  serial: 0\n"), vec![]);
    }

    #[test]
    fn serial_boundaries() {
        let play = |v: &str| run(&format!("- hosts: all\n  serial: {v}\n  tasks: []\n"));
        assert_eq!(play("-1"), vec![("-1".into(), Tier::Hint)]);
        assert_eq!(play("\"-10%\""), vec![("-10%".into(), Tier::Hint)]);
        // Controls: the smallest real batch, and a percentage above zero.
        assert_eq!(play("1"), vec![]);
        assert_eq!(play("\"1%\""), vec![]);
        assert_eq!(play("\"{{ n }}\""), vec![]);
    }

    fn messages(src: &str) -> Vec<String> {
        let nodes = Document::new(src.into()).parse().unwrap();
        problems(&crate::ast::build(&nodes), &nodes).into_iter().map(|p| p.message).collect()
    }

    /// Every legal value passes, so the tables cannot drift from Ansible's by a typo.
    #[test]
    fn every_legal_value_passes() {
        for v in keywords::DEBUGGER_VALUES {
            assert_eq!(run(&format!("- debug: msg=x\n  debugger: {v}\n")), vec![], "debugger: {v}");
        }
        for v in keywords::ORDER_VALUES {
            assert_eq!(run(&format!("- hosts: all\n  order: {v}\n  tasks: []\n")), vec![], "order: {v}");
        }
    }

    /// Measured: `Always` and `Sorted` both fail on 2.21.3.
    #[test]
    fn values_are_case_sensitive() {
        assert_eq!(run("- debug: msg=x\n  debugger: Always\n"), vec![("Always".into(), Tier::Error)]);
        assert_eq!(
            run("- hosts: all\n  order: Sorted\n  tasks: []\n"),
            vec![("Sorted".into(), Tier::Error)]
        );
    }

    /// Measured: both keys render before they are checked.
    #[test]
    fn templated_values_are_skipped() {
        assert_eq!(run("- debug: msg=x\n  debugger: \"{{ d }}\"\n"), vec![]);
        assert_eq!(run("- hosts: all\n  order: \"{{ o }}\"\n  tasks: []\n"), vec![]);
    }

    /// Each level is checked, and the message says what fails: one task, or every task
    /// under a play or block.
    #[test]
    fn debugger_is_checked_at_every_level_with_the_right_scope() {
        let src = "- hosts: all\n  debugger: bad1\n  tasks:\n    - block:\n        - debug: msg=x\n          debugger: bad3\n      debugger: bad2\n";
        let got = run(src);
        assert_eq!(
            got,
            vec![("bad1".into(), Tier::Error), ("bad2".into(), Tier::Error), ("bad3".into(), Tier::Error)]
        );
        let m = messages(src);
        assert!(m[0].contains("every task it applies to fails"), "play: {}", m[0]);
        assert!(m[1].contains("every task it applies to fails"), "block: {}", m[1]);
        assert!(m[2].contains("this task fails"), "task: {}", m[2]);
        assert!(m[2].contains("on_unreachable"), "legal values listed: {}", m[2]);
    }

    /// `order` is a play keyword only; on a task it is an invalid attribute, not this rule.
    #[test]
    fn order_off_a_play_is_not_judged() {
        assert_eq!(run("- debug: msg=x\n  order: random\n"), vec![]);
    }

    /// The three `serial` hints say three different things; a list item says "remaining".
    #[test]
    fn serial_messages_name_what_actually_happens() {
        let play = |v: &str| messages(&format!("- hosts: all\n  serial: {v}\n  tasks: []\n"));
        assert!(play("0")[0].contains("every host in one batch"), "{:?}", play("0"));
        assert!(play("\"0%\"")[0].contains("batch of one host"), "{:?}", play("\"0%\""));
        let list = play("[1, 0]");
        assert_eq!(list.len(), 1, "only the 0 item: {list:?}");
        assert!(list[0].contains("all remaining hosts"), "{list:?}");
    }
}
