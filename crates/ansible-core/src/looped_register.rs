//! T-193: a looped task's register has no module keys, only `results`.
//!
//! Measured on ansible-core 2.21.3: a task with `loop:` or `with_*` registers
//! `changed, failed, msg, results, warnings` (plus `skipped` when every item skipped), and the
//! per-item results sit under `.results`. So `{{ r.stdout }}` after the loop fails the task
//! reading it — "object of type 'dict' has no attribute 'stdout'" — in a template, a later
//! task's `when:` or `failed_when:`, and the `r['stdout']` spelling alike.
//!
//! Not flagged, each measured working: reads through `results`, the aggregate keys above,
//! `is defined` / `is not defined`, a `default` / `d` filter, and a method call (`r.get(...)`).
//!
//! Reads inside the registering task are left alone. Its `failed_when`, `changed_when` and
//! `until` see the current item's result, which does have `rc`. Its `when`, `retries`, `delay`
//! and args are rendered before each item — `r` is undefined on the first, and something
//! else again after — so they fail too, but for a reason this rule's message would get wrong.
//!
//! Only a name registered once in the file, by that looped task, and defined nowhere else,
//! is judged: with two definitions, which one a read sees depends on what ran.

use crate::ast::{Ast, PlayItem, Stmt};
use crate::parse::{Node, Span};

/// Rule id, for `# noqa: looped-register-key` and for display.
pub const RULE_ID: &str = "looped-register-key";

/// What a looped register does carry (measured on 2.21.3).
const AGGREGATE_KEYS: &[&str] = &["changed", "failed", "msg", "results", "skipped", "warnings"];

#[derive(Debug, Clone)]
pub struct Problem {
    /// The key read off the register: `stdout` in `r.stdout`.
    pub span: Span,
    pub message: String,
    pub rule: &'static str,
}

pub fn problems(ast: &Ast, nodes: &[Node], text: &str) -> Vec<Problem> {
    let mut looped = Vec::new();
    match ast {
        Ast::Playbook(items) => {
            for item in items {
                let PlayItem::Play(p) = item else { continue };
                for s in p.pre_tasks.iter().chain(&p.tasks).chain(&p.post_tasks).chain(&p.handlers) {
                    collect(s, &mut looped);
                }
            }
        }
        Ast::Tasks(stmts) => stmts.iter().for_each(|s| collect(s, &mut looped)),
        Ast::Other => {}
    }
    if looped.is_empty() {
        return Vec::new();
    }
    let index = crate::vars::index(ast);
    let uses = crate::vars::any_uses(nodes);
    let mut out = Vec::new();
    for (name, task) in looped {
        if index.get(&name).len() != 1 {
            continue;
        }
        for u in uses.iter().filter(|u| u.name == name && u.span.start >= task.end) {
            let Some((key, span, rest)) = accessed_key(text, u.span.end) else { continue };
            if AGGREGATE_KEYS.contains(&key) || guarded(rest) {
                continue;
            }
            out.push(Problem {
                span,
                message: format!(
                    "`{name}` is registered by a looped task, so it has no `{key}` — only \
                     `results`, one entry per item. Reading `{name}.{key}` fails the task \
                     (\"object of type 'dict' has no attribute '{key}'\"). Read it per item: \
                     `{name}.results[0].{key}`, or `{name}.results | map(attribute='{key}')`."
                ),
                rule: RULE_ID,
            });
        }
    }
    out
}

/// `(register name, registering task's span)` for every looped task with a `register:`.
fn collect(s: &Stmt, out: &mut Vec<(String, Span)>) {
    match s {
        Stmt::Block(b) => b.block.iter().chain(&b.rescue).chain(&b.always).for_each(|c| collect(c, out)),
        Stmt::Task(t) => {
            if let (true, Some(r)) = (t.looped, &t.register) {
                out.push((r.clone(), t.span));
            }
        }
    }
}

/// The key read right after a use ending at `at` — `.key` or `['key']` / `["key"]` — with its
/// span and the text after the accessor.
fn accessed_key(text: &str, at: usize) -> Option<(&str, Span, &str)> {
    let rest = &text[at..];
    let ident_len = |s: &str| s.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(s.len());
    if let Some(after_dot) = rest.strip_prefix('.') {
        let n = ident_len(after_dot);
        if n == 0 {
            return None;
        }
        let start = at + 1;
        return Some((&text[start..start + n], Span { start, end: start + n }, &after_dot[n..]));
    }
    let inner = rest.strip_prefix('[')?.trim_start();
    let quote = inner.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let body = &inner[1..];
    let n = body.find(quote)?;
    let start = text.len() - body.len();
    let after = body[n + 1..].trim_start().strip_prefix(']')?;
    Some((&text[start..start + n], Span { start, end: start + n }, after))
}

/// The read cannot fail: a method call on the register, or the missing key absorbed by
/// `is defined` / `is not defined` / `is undefined` or a `default` / `d` filter. Checked after
/// any further `.x` / `[...]` chain on the key.
fn guarded(rest: &str) -> bool {
    if rest.starts_with('(') {
        return true;
    }
    let mut r = rest;
    loop {
        if let Some(t) = r.strip_prefix('.') {
            r = &t[t.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(t.len())..];
        } else if r.starts_with('[') {
            let Some(close) = r.find(']') else { break };
            r = &r[close + 1..];
        } else {
            break;
        }
    }
    let r = r.trim_start();
    if let Some(t) = r.strip_prefix("is") {
        let t = t.trim_start();
        let t = t.strip_prefix("not").map(str::trim_start).unwrap_or(t);
        return t.starts_with("defined") || t.starts_with("undefined");
    }
    if let Some(t) = r.strip_prefix('|') {
        let t = t.trim_start();
        let word = &t[..t.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(t.len())];
        return word == "default" || word == "d";
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    /// The text of each flagged key.
    fn keys(src: &str) -> Vec<String> {
        let nodes = Document::new(src.into()).parse().unwrap();
        problems(&crate::ast::build(&nodes), &nodes, src)
            .into_iter()
            .map(|p| p.span.slice(src).to_string())
            .collect()
    }

    const LOOPED: &str = "- hosts: all\n  tasks:\n    - command: echo {{ item }}\n      loop: [a, b]\n      register: r\n";

    fn after(rest: &str) -> Vec<String> {
        keys(&format!("{LOOPED}{rest}"))
    }

    #[test]
    fn a_module_key_after_the_loop_fires() {
        assert_eq!(after("    - debug: msg=\"{{ r.stdout }}\"\n"), vec!["stdout"]);
    }

    #[test]
    fn the_bracket_spelling_fires() {
        assert_eq!(after("    - debug: msg=\"{{ r['stdout'] }}\"\n"), vec!["stdout"]);
        assert_eq!(after("    - debug:\n        msg: '{{ r[\"rc\"] }}'\n"), vec!["rc"]);
    }

    /// Measured: a later task's `when:` reads the aggregate and crashes.
    #[test]
    fn a_later_tasks_when_fires() {
        assert_eq!(after("    - debug: msg=x\n      when: r.rc == 0\n"), vec!["rc"]);
    }

    /// Measured: a later task's `failed_when:` crashes the same way. The variable walk does not
    /// read `failed_when` / `changed_when` / `until` yet, so no use reaches this rule.
    #[test]
    #[ignore = "vars::any_uses does not read failed_when/changed_when/until yet — T-122"]
    fn a_later_tasks_failed_when_fires() {
        assert_eq!(after("    - debug: msg=x\n      failed_when: r.rc != 0\n"), vec!["rc"]);
    }

    #[test]
    fn with_items_behaves_as_loop() {
        let src = "- hosts: all\n  tasks:\n    - command: echo {{ item }}\n      with_items: [a, b]\n      register: r\n    - debug: msg=\"{{ r.stdout }}\"\n";
        assert_eq!(keys(src), vec!["stdout"]);
    }

    /// A role's task file is checked too.
    #[test]
    fn a_task_file_is_checked() {
        let src = "- command: echo {{ item }}\n  loop: [a]\n  register: r\n- debug: msg=\"{{ r.stdout }}\"\n";
        assert_eq!(keys(src), vec!["stdout"]);
    }

    /// The control: the same reads through `results` are correct.
    #[test]
    fn reads_through_results_are_silent() {
        assert_eq!(after("    - debug: msg=\"{{ r.results[0].stdout }}\"\n"), Vec::<String>::new());
        assert_eq!(
            after("    - debug: msg=\"{{ r.results | map(attribute='stdout') | list }}\"\n"),
            Vec::<String>::new()
        );
    }

    /// Measured on 2.21.3: these are the keys a looped register does have.
    #[test]
    fn the_aggregate_keys_are_silent() {
        for k in ["changed", "failed", "msg", "results", "skipped", "warnings"] {
            assert_eq!(after(&format!("    - debug: msg=\"{{{{ r.{k} }}}}\"\n")), Vec::<String>::new(), "{k}");
        }
    }

    /// Measured: none of these crash.
    #[test]
    fn guarded_reads_are_silent() {
        for read in ["r.stdout is defined", "r.stdout is not defined", "r.stdout | default('')", "r.stdout|d('')", "r.get('stdout')"] {
            assert_eq!(after(&format!("    - debug: msg=\"{{{{ {read} }}}}\"\n")), Vec::<String>::new(), "{read}");
        }
    }

    /// Inside the registering task, `failed_when` / `changed_when` / `until` see the item's own
    /// result (measured: `failed=0`). Its `when` / `retries` / `delay` and args are rendered
    /// before each item and crash differently — not this rule's claim. One assertion each.
    #[test]
    fn reads_inside_the_registering_task_are_silent() {
        for kw in [
            "failed_when: r.rc != 0",
            "changed_when: \"'a' in r.stdout\"",
            "until: r.rc == 0",
            "when: r.rc == 0",
            "retries: \"{{ r.rc }}\"",
            "delay: \"{{ r.rc }}\"",
        ] {
            let src = format!("- hosts: all\n  tasks:\n    - command: echo {{{{ item }}}}\n      loop: [a, b]\n      register: r\n      {kw}\n");
            assert_eq!(keys(&src), Vec::<String>::new(), "{kw}");
        }
    }

    #[test]
    fn an_unlooped_register_is_silent() {
        let src = "- hosts: all\n  tasks:\n    - command: echo a\n      register: r\n    - debug: msg=\"{{ r.stdout }}\"\n";
        assert_eq!(keys(src), Vec::<String>::new());
    }

    /// Registered twice — once looped, once not — which one a read sees depends on order and
    /// on which ran. Too uncertain to claim.
    #[test]
    fn a_name_defined_twice_is_silent() {
        let src = format!("{LOOPED}    - command: echo b\n      register: r\n    - debug: msg=\"{{{{ r.stdout }}}}\"\n");
        assert_eq!(keys(&src), Vec::<String>::new());
        let src = format!("{LOOPED}    - set_fact:\n        r: {{stdout: x}}\n    - debug: msg=\"{{{{ r.stdout }}}}\"\n");
        assert_eq!(keys(&src), Vec::<String>::new());
    }

    /// Before the registering task runs, `r` is not this task's register.
    #[test]
    fn a_read_before_the_registering_task_is_silent() {
        let src = "- hosts: all\n  tasks:\n    - debug: msg=\"{{ r.stdout }}\"\n    - command: echo {{ item }}\n      loop: [a]\n      register: r\n";
        assert_eq!(keys(src), Vec::<String>::new());
    }

    /// A name that merely starts with the register's is a different variable.
    #[test]
    fn a_longer_name_is_not_the_register() {
        assert_eq!(after("    - debug: msg=\"{{ rr.stdout }} {{ r_x.stdout }}\"\n"), Vec::<String>::new());
    }

    #[test]
    fn the_message_names_the_key_and_the_fix() {
        let src = format!("{LOOPED}    - debug: msg=\"{{{{ r.stdout }}}}\"\n");
        let nodes = Document::new(src.clone()).parse().unwrap();
        let p = &problems(&crate::ast::build(&nodes), &nodes, &src)[0];
        assert_eq!(p.rule, RULE_ID);
        assert!(p.message.contains("`stdout`") && p.message.contains("results"), "{}", p.message);
    }
}
