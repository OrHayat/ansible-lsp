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
use crate::parse::{node_with_span, Node, Span};

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
        Stmt::Task(t) => {
            directives(&t.directives, Scope::Task, nodes, out);
            variable_names(&t.directives, nodes, out);
        }
    }
}

/// T-108: `register`, `loop_var` and `index_var` must pass `validate_variable_name`.
/// Measured on 2.21.3, the two kinds fail differently:
///
/// - `register` is checked when the task loads, before anything renders — `--syntax-check`
///   fails. The dict form checks each key. Null is fine. A templated name is fatal too, but
///   [`crate::static_fields`] reports that, so it is skipped here rather than said twice.
/// - `loop_var` / `index_var` are checked when the task runs, after templating, so a
///   `{{ }}` value is skipped. A false `when:` on the task or its block does not prevent it;
///   `ignore_errors` and `rescue` catch it like any task failure.
///
/// The parser drops scalar style (T-162), so a plain YAML 1.1 bool like `register: yes`
/// reads as the string `yes` and passes — a silence, never a false error.
fn variable_names(ds: &[Directive], nodes: &[Node], out: &mut Vec<Problem>) {
    if let Some(value) = last(ds, "register", nodes) {
        let names: Vec<&Node> = match value {
            Node::Mapping { entries, .. } => entries.iter().map(|(k, _)| k).collect(),
            _ => vec![value],
        };
        for n in names {
            // `~` is YAML null, which Ansible accepts.
            let Some(v) = literal(n).filter(|v| *v != "~") else { continue };
            if !is_variable_name(v) {
                let message = format!(
                    "`{v}` is not a valid `register` name — Ansible refuses to load this task, \
                     and `--syntax-check` fails. {}",
                    name_rule(v)
                );
                out.push(Problem { span: n.span(), tier: Tier::Error, message, rule: RULE_ID });
            }
        }
    }
    let Some(Node::Mapping { entries, .. }) = last(ds, "loop_control", nodes) else { return };
    for key in ["loop_var", "index_var"] {
        let Some((_, value)) = entries.iter().rev().find(|(k, _)| k.as_str() == Some(key)) else { continue };
        let Some(v) = literal(value) else { continue };
        if !is_variable_name(v) {
            let message = format!(
                "`{v}` is not a valid `{key}` — this task fails whenever it runs, even under a \
                 false `when:`, though `--syntax-check` passes. {}",
                name_rule(v)
            );
            out.push(Problem { span: value.span(), tier: Tier::Error, message, rule: RULE_ID });
        }
    }
}

/// On a duplicate key Ansible loads only the last.
fn last<'a>(ds: &[Directive], key: &str, nodes: &'a [Node]) -> Option<&'a Node> {
    let d = ds.iter().rev().find(|d| d.key == key)?;
    node_with_span(nodes, d.value)
}

/// `validate_variable_name` (`utils/vars.py`): `isidentifier()` and `isascii()`, minus the
/// names Jinja reserves (`_jinja_bits.JINJA_KEYWORDS`). Python keywords like `class` pass.
pub fn is_variable_name(v: &str) -> bool {
    let mut chars = v.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !JINJA_KEYWORDS.contains(&v)
}

const JINJA_KEYWORDS: &[&str] = &["true", "false", "none", "True", "False", "None", "not"];

fn name_rule(v: &str) -> &'static str {
    if JINJA_KEYWORDS.contains(&v) {
        "Jinja reserves this name."
    } else {
        "A name starts with a letter or underscore and holds only ASCII letters, digits and \
         underscores."
    }
}

fn directives(ds: &[Directive], scope: Scope, nodes: &[Node], out: &mut Vec<Problem>) {
    for key in ["debugger", "order", "serial"] {
        // On a duplicate key Ansible loads only the last, so only the last is judged.
        let Some(d) = ds.iter().rev().find(|d| d.key == key) else { continue };
        let Some(value) = node_with_span(nodes, d.value) else { continue };
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

    /// T-108, each row measured on 2.21.3: the rejected shapes fail `--syntax-check` with
    /// "Invalid 'register' specified", the accepted ones run.
    #[test]
    fn register_names_follow_validate_variable_name() {
        let reg = |v: &str| run(&format!("- debug: msg=x\n  register: {v}\n"));
        for bad in ["my-var", "none", "not", "True", "café", "42", "\"\"", "\"a b\""] {
            assert_eq!(reg(bad).len(), 1, "register: {bad}");
        }
        for good in ["my_var", "_x", "x1", "class", "null", "~", "\"Null\""] {
            assert_eq!(reg(good), vec![], "register: {good}");
        }
        // Fatal too, but `static-template`'s report; one claim per token.
        assert_eq!(reg("\"{{ x }}\""), vec![]);
        // The dict form checks each key, and only the key.
        let got = reg("\n    ok_name: _task.result.msg\n    bad-name: _task.result.msg");
        assert_eq!(got, vec![("bad-name".into(), Tier::Error)]);
    }

    /// Measured: both render first and fail at run time, so a template is skipped; a bad
    /// literal fails the task.
    #[test]
    fn loop_var_and_index_var_are_checked_after_templating() {
        let lc = |k: &str, v: &str| run(&format!("- debug: msg=x\n  loop: [1]\n  loop_control:\n    {k}: {v}\n"));
        assert_eq!(lc("loop_var", "my-item"), vec![("my-item".into(), Tier::Error)]);
        assert_eq!(lc("loop_var", "True"), vec![("True".into(), Tier::Error)]);
        assert_eq!(lc("index_var", "5"), vec![("5".into(), Tier::Error)]);
        assert_eq!(lc("loop_var", "my_item"), vec![]);
        assert_eq!(lc("loop_var", "\"{{ v }}\""), vec![]);
        assert_eq!(lc("index_var", "idx"), vec![]);
        // `label` and `pause` are not names.
        assert_eq!(lc("label", "my-label"), vec![]);
    }

    /// The two kinds fail at different times, and the message must say which.
    #[test]
    fn name_messages_say_when_ansible_fails() {
        let m = messages("- debug: msg=x\n  register: my-var\n  loop: [1]\n  loop_control: {loop_var: my-item}\n");
        assert_eq!(m.len(), 2, "{m:?}");
        assert!(m[0].contains("`--syntax-check` fails"), "{}", m[0]);
        assert!(m[1].contains("`--syntax-check` passes") && m[1].contains("false `when:`"), "{}", m[1]);
        assert!(messages("- debug: msg=x\n  register: none\n")[0].contains("Jinja reserves"));
    }

    /// Corpus gate. Prints every hit, beside how many keys of each kind the sweep saw — zero
    /// hits over zero keys is a sweep that looked at nothing. An error-tier hit on working
    /// code means the rule is wrong, since it claims the task cannot run.
    #[test]
    #[ignore = "corpus gate: ANSIBLE_CORPUS=<path> cargo test -p ansible-core keyword_values_corpus -- --ignored --nocapture"]
    fn keyword_values_corpus() {
        let Ok(root) = std::env::var("ANSIBLE_CORPUS") else { return };
        let root = std::path::PathBuf::from(root);
        let files = crate::workspace::yaml_files(&root);
        assert!(!files.is_empty(), "no YAML under {}", root.display());
        let mut seen: std::collections::BTreeMap<&str, usize> = Default::default();
        let mut hits = 0usize;
        for path in &files {
            let Ok(text) = std::fs::read_to_string(path) else { continue };
            for key in ["register:", "loop_var:", "index_var:", "debugger:", "order:", "serial:"] {
                *seen.entry(key).or_default() += text.matches(key).count();
            }
            let Some(nodes) = Document::new(text.clone()).parse() else { continue };
            for p in problems(&crate::ast::build(&nodes), &nodes) {
                let line = text[..p.span.start.min(text.len())].lines().count();
                hits += 1;
                println!("  HIT {:?} {}:{} {}", p.tier, path.strip_prefix(&root).unwrap_or(path).display(), line, p.message);
            }
        }
        println!("--- {} files, {hits} hit(s), keys seen {seen:?}", files.len());
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
