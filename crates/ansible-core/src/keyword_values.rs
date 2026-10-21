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

use crate::ast::{Ast, Directive, PlayItem, RoleUse, Stmt, Task, UnknownKey};
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

impl Scope {
    fn noun(self) -> &'static str {
        match self {
            Scope::Play => "play",
            Scope::Block => "block",
            Scope::Task => "task",
        }
    }
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
                listof(&p.directives, &p.unknown_keys, None, nodes, &mut out);
                vars_keys(&p.directives, Scope::Play, true, nodes, &mut out);
                for r in &p.roles {
                    role_entry_vars(r, nodes, &mut out);
                }
                for s in p.pre_tasks.iter().chain(&p.tasks).chain(&p.post_tasks).chain(&p.handlers) {
                    stmt(s, true, nodes, &mut out);
                }
            }
        }
        Ast::Tasks(stmts) => {
            for s in stmts {
                stmt(s, false, nodes, &mut out);
            }
        }
        Ast::Other => {}
    }
    out
}

fn stmt(s: &Stmt, playbook: bool, nodes: &[Node], out: &mut Vec<Problem>) {
    match s {
        Stmt::Block(b) => {
            directives(&b.directives, Scope::Block, nodes, out);
            listof(&b.directives, &b.unknown_keys, None, nodes, out);
            vars_keys(&b.directives, Scope::Block, playbook, nodes, out);
            for child in b.block.iter().chain(&b.rescue).chain(&b.always) {
                stmt(child, playbook, nodes, out);
            }
        }
        Stmt::Task(t) => {
            directives(&t.directives, Scope::Task, nodes, out);
            listof(&t.directives, &t.unknown_keys, node_with_span(nodes, t.span), nodes, out);
            variable_names(&t.directives, playbook, nodes, out);
            vars_keys(&t.directives, Scope::Task, playbook, nodes, out);
            set_fact_keys(t, out);
        }
    }
}

/// What a name checked at load takes down. Measured on 2.21.3: in a playbook, or a task file
/// reached by `import_tasks`, `--syntax-check` fails; in a file reached by `include_tasks` it
/// passes, and the include itself fails when it runs.
fn load_failure(scope: Scope, playbook: bool) -> String {
    if playbook {
        format!("Ansible refuses to load this {}, and `--syntax-check` fails", scope.noun())
    } else {
        "this file fails to load wherever it is imported or included".into()
    }
}

/// T-247: every key of a `vars:` mapping goes through `validate_variable_name` when the
/// play, block or task loads (`base.py:_load_vars`), so it fails like a bad `register`. A
/// templated key fails too, but [`crate::static_fields`] reports that. A null key is fatal
/// with a different message and is left alone.
fn vars_keys(ds: &[Directive], scope: Scope, playbook: bool, nodes: &[Node], out: &mut Vec<Problem>) {
    if let Some(vars) = last(ds, "vars", nodes) {
        vars_mapping(vars, &load_failure(scope, playbook), out);
    }
}

/// A `roles:` entry's `vars:` loads with the play, so a bad key takes the play down.
fn role_entry_vars(r: &RoleUse, nodes: &[Node], out: &mut Vec<Problem>) {
    let Some(entry) = node_with_span(nodes, r.entry_span) else { return };
    if let Some((_, vars)) = entry.entries().iter().rev().find(|(k, _)| k.as_str() == Some("vars")) {
        vars_mapping(vars, &load_failure(Scope::Play, true), out);
    }
}

/// A mapping's keys as Ansible sees them. `<<` is YAML's merge key: the loader expands it
/// instead of keeping it, measured for `vars:`, `set_fact` and `register`, while our parser
/// keeps it as written.
fn keys(m: &Node) -> impl Iterator<Item = &Node> {
    m.entries().iter().map(|(k, _)| k).filter(|k| k.as_str() != Some("<<"))
}

fn vars_mapping(vars: &Node, fails: &str, out: &mut Vec<Problem>) {
    if !matches!(vars, Node::Mapping { .. }) {
        return;
    }
    for k in keys(vars) {
        let Some(v) = literal(k) else { continue };
        if !is_variable_name(v) {
            let message = format!("`{v}` is not a valid variable name — {fails}. {}", name_rule(v));
            out.push(Problem { span: k.span(), tier: Tier::Error, message, rule: RULE_ID });
        }
    }
}

/// T-247: `set_fact` renders each key, then validates it (`plugins/action/set_fact.py:46`), so
/// a literal bad key fails the task when it runs, and a false `when:` skips it. `cacheable` is
/// an option, not a fact. Mapping args only; the `k=v` spelling is T-046's to split.
fn set_fact_keys(t: &Task, out: &mut Vec<Problem>) {
    let Some(a) = &t.action else { return };
    if keywords::core_action(&a.name) != "set_fact" {
        return;
    }
    for k in keys(&a.args) {
        let Some(v) = literal(k).filter(|v| *v != "cacheable") else { continue };
        if !is_variable_name(v) {
            let message = format!(
                "`{v}` is not a valid variable name — this task fails when it runs, though \
                 `--syntax-check` passes. {}",
                name_rule(v)
            );
            out.push(Problem { span: k.span(), tier: Tier::Error, message, rule: RULE_ID });
        }
    }
}

/// T-108: `register`, `loop_var` and `index_var` must pass `validate_variable_name`.
/// Measured on 2.21.3, the two kinds fail differently:
///
/// - `register` is checked when the task loads, before anything renders — see
///   [`load_failure`] for what that takes down. The dict form checks each key. Null is fine.
///   A templated name is fatal too, but [`crate::static_fields`] reports that, so it is
///   skipped here rather than said twice.
/// - `loop_var` / `index_var` are checked when the task runs, after templating, so a
///   `{{ }}` value is skipped. A false `when:` on the task or its block does not prevent it;
///   `ignore_errors` and `rescue` catch it like any task failure.
///
/// The parser drops scalar style (T-162), so a plain YAML 1.1 bool like `register: yes`
/// reads as the string `yes` and passes — a silence, never a false error.
fn variable_names(ds: &[Directive], playbook: bool, nodes: &[Node], out: &mut Vec<Problem>) {
    if let Some(value) = last(ds, "register", nodes) {
        let names: Vec<&Node> = match value {
            Node::Mapping { .. } => keys(value).collect(),
            _ => vec![value],
        };
        for n in names {
            // `~` is YAML null, which Ansible accepts.
            let Some(v) = literal(n).filter(|v| *v != "~") else { continue };
            if !is_variable_name(v) {
                let message = format!(
                    "`{v}` is not a valid `register` name — {}. {}",
                    load_failure(Scope::Task, playbook),
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

/// T-108: `listof` (`base.py:483-491`), for the three keywords no other rule covers — `hosts`
/// is `placement::hosts`, and a container in `tags` crashes before `listof` runs, which is
/// `placement::tags_checks`. Measured on 2.21.3, all fail `--syntax-check` with Ansible's
/// "Keyword 'listen' items must be of type 'str', not 'dict'." A lone mapping is wrapped into
/// a one-item list first, so it fails the same way; a null *item* is `NoneType`, while a null
/// value is an empty list and fine.
///
/// The parser drops scalar style (T-162), so `[1]` reads as `["1"]` and only containers and
/// nulls can be judged. A key the invalid-attribute rule already rejects here (`listen:` on a
/// plain task, `gather_subset:` on a block) is that rule's report, not a second one.
///
/// `listen` is structural on a task, so not among its directives; `task` is its own mapping.
fn listof(ds: &[Directive], unknown: &[UnknownKey], task: Option<&Node>, nodes: &[Node], out: &mut Vec<Problem>) {
    for key in ["collections", "gather_subset", "listen"] {
        if unknown.iter().any(|u| u.key == key) {
            continue;
        }
        let entry = match (key, task) {
            ("listen", Some(t)) => t.entries().iter().rev().find(|(k, _)| k.as_str() == Some(key)).map(|(k, v)| (k.span(), v)),
            _ => ds.iter().rev().find(|d| d.key == key).and_then(|d| Some((d.key_span, node_with_span(nodes, d.value)?))),
        };
        let Some((key_span, value)) = entry else { continue };
        let items: Vec<&Node> = match value {
            Node::Sequence { items, .. } => items.iter().collect(),
            Node::Mapping { .. } => vec![value],
            _ => continue,
        };
        for item in items {
            let found = match item {
                Node::Mapping { .. } => "dict",
                Node::Sequence { .. } => "list",
                Node::Null { .. } => "NoneType",
                _ => continue,
            };
            let message = format!(
                "Keyword '{key}' items must be of type 'str', not '{found}'. Ansible refuses to \
                 load this, and `--syntax-check` fails."
            );
            // A null item's span is empty; point at the key instead.
            let span = if matches!(item, Node::Null { .. }) { key_span } else { item.span() };
            out.push(Problem { span, tier: Tier::Error, message, rule: RULE_ID });
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
    (!crate::jinja::is_template(v)).then_some(v)
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
        assert!(m[0].contains("wherever it is imported or included"), "{}", m[0]);
        assert!(m[1].contains("`--syntax-check` passes") && m[1].contains("false `when:`"), "{}", m[1]);
        assert!(messages("- debug: msg=x\n  register: none\n")[0].contains("Jinja reserves"));
        let play = messages("- hosts: all\n  tasks:\n    - debug: msg=x\n      register: my-var\n");
        assert!(play[0].contains("refuses to load this task, and `--syntax-check` fails"), "{}", play[0]);
    }

    #[test]
    fn vars_keys_are_checked_at_play_block_and_task() {
        let src = "- hosts: all\n  vars: {play-var: 1, ok_play: 1}\n  tasks:\n    \
                   - block:\n        - debug: msg=x\n          vars: {task-var: 1, class: 1}\n      \
                   vars: {none: 1, '1': 1, \"{{ t }}\": 1}\n";
        let got: Vec<String> = run(src).into_iter().map(|(s, _)| s).collect();
        assert_eq!(got, ["play-var", "none", "1", "task-var"], "a templated key is static-template's");
        let m = messages(src);
        assert!(m[0].contains("refuses to load this play"), "{}", m[0]);
        assert!(m[1].contains("refuses to load this block") && m[1].contains("Jinja reserves"), "{}", m[1]);
        assert!(m[3].contains("refuses to load this task"), "{}", m[3]);
        let roles = "- hosts: all\n  roles:\n    - role: r\n      vars: {role-var: 1, ok: 1}\n    - r2\n";
        assert_eq!(run(roles), vec![("role-var".into(), Tier::Error)]);
        assert!(messages(roles)[0].contains("refuses to load this play"));
        // A merge key is expanded by the loader, not validated, at every site that reads keys.
        let merge = "- hosts: all\n  vars:\n    b: &b {x: 1}\n    <<: *b\n  tasks:\n    \
                     - set_fact:\n        <<: *b\n    - debug: msg=x\n      register:\n        <<: *b\n";
        assert_eq!(run(merge), vec![]);
        // Only the last of duplicate `vars:` keys loads.
        assert_eq!(run("- debug: msg=x\n  vars: {a-b: 1}\n  vars: {ok: 1}\n"), vec![]);
    }

    #[test]
    fn set_fact_keys_fail_at_run_time() {
        let src = "- set_fact: {my-fact: 1, ok_fact: 1, cacheable: true, \"{{ k }}\": 1}\n\
                   - ansible.builtin.set_fact:\n    other-fact: 1\n\
                   - uri: {url: x, return-content: true}\n";
        let got: Vec<String> = run(src).into_iter().map(|(s, _)| s).collect();
        assert_eq!(got, ["my-fact", "other-fact"]);
        let m = messages(src);
        assert!(m[0].contains("fails when it runs") && m[0].contains("`--syntax-check` passes"), "{}", m[0]);
    }

    /// T-108's `listof`, each row measured fatal on 2.21.3 with Ansible's own message.
    #[test]
    fn listof_keywords_refuse_container_and_null_items() {
        let play = |v: &str| run(&format!("- hosts: all\n  {v}\n  tasks: []\n"));
        assert_eq!(play("gather_subset: [min, {a: 1}]"), vec![("{a: 1}".into(), Tier::Error)]);
        assert_eq!(play("gather_subset: [[min]]"), vec![("[min]".into(), Tier::Error)]);
        assert_eq!(play("collections: {a: 1}"), vec![("{a: 1}".into(), Tier::Error)]);
        // A bare dash is a null item, anchored on the key since it has no text of its own.
        assert_eq!(play("collections:\n    - ns.coll\n    -"), vec![("collections".into(), Tier::Error)]);
        let task = |v: &str| run(&format!("- debug: msg=x\n  {v}\n"));
        assert_eq!(task("collections: [{a: 1}]"), vec![("{a: 1}".into(), Tier::Error)]);
        // A task file reads as handlers, where `listen` is legal.
        assert_eq!(task("listen: [t, {a: 1}]"), vec![("{a: 1}".into(), Tier::Error)]);
        // And under a play's `handlers:`, the case that matters.
        let h = run("- hosts: all\n  tasks: []\n  handlers:\n    - debug: msg=x\n      listen: [t, [u]]\n");
        assert_eq!(h, vec![("[u]".into(), Tier::Error)]);
        let m = messages("- debug: msg=x\n  listen: [{a: 1}]\n");
        assert!(m[0].starts_with("Keyword 'listen' items must be of type 'str', not 'dict'."), "{}", m[0]);
    }

    #[test]
    fn listof_controls_stay_silent() {
        let play = |v: &str| run(&format!("- hosts: all\n  {v}\n  tasks: []\n"));
        for ok in ["gather_subset: [min, network]", "gather_subset: min", "gather_subset:", "collections: [a.b]", "gather_subset: [\"{{ s }}\"]"] {
            assert_eq!(play(ok), vec![], "{ok}");
        }
        // `listen` on a play's task (not a handler) is likewise that rule's.
        assert_eq!(run("- hosts: all\n  tasks:\n    - debug: msg=x\n      listen: [{a: 1}]\n"), vec![]);
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
