//! T-170: a template in a module argument key is never rendered.
//!
//! Measured on ansible-core 2.21.2. Exactly two action plugins render a mapping key —
//! `set_fact`'s own args and `set_stats`' `data:`, which upstream calls "a rare case where
//! key templating is allowed" (`action/set_fact.py:44`). Everywhere else the braces are
//! text, and the author who wrote them gets nothing they intended.
//!
//! The control that settles it: `debug: {"{{ p }}": x}` with `p` bound to `msg`. If keys
//! were rendered this would become `msg:` and print. It fails with
//! `Unsupported parameters for (debug) module: {{ p }}` — the literal braces reach the
//! module as a parameter name, so nothing was rendered.
//!
//! What happens next is not one behaviour but four, which is why this rule enumerates
//! rather than guessing from the module:
//!
//! | spelling | measured |
//! | --- | --- |
//! | `debug: {"{{ k }}": v}` | fatal, `Unsupported parameters for (debug) module: {{ k }}` |
//! | `group_by: {key: g, "{{ k }}": v}` | fatal, `Invalid options for group_by: {{ k }}` |
//! | `add_host: {name: h, "{{ k }}": v}` | **runs**; the host var is literally named `{{ k }}` |
//! | `command: {"{{ k }}": v}` | fatal, but the key is *dropped*: `one of the following is required: _raw_params, cmd, argv` |
//!
//! `add_host` is the worst of them: it takes arbitrary keys as host vars, so it looks like a
//! third rendering site. The intended variable is never set, and the one that is set cannot
//! be named by any expression — read back from a later play, the host's keys are `['{{ k }}']`.
//!
//! The free-form row is why [`FREE_FORM`] exists. Those modules swallow the unknown key and
//! fail for a different reason, so naming `Unsupported parameters` there would be a
//! confident wrong answer about a real failure. They are skipped rather than reported with
//! a message that does not match what the user will see.
//!
//! Only **top-level** arg keys are judged. A key nested inside a value is ordinary data
//! where braces are legal and mean nothing — measured: `debug: {msg: {"{{ k }}": v}}` runs
//! and prints the literal key. That is the same boundary T-169's walk already draws.

use crate::ast::{Ast, PlayItem, Stmt, Task};
use crate::attributes::Tier;
use crate::keywords;
use crate::parse::{Node, Span};

/// Rule id, for `# noqa: templated-arg-key` and for display.
pub const RULE_ID: &str = "templated-arg-key";

/// The two action plugins that really do render their keys — the control for this rule.
/// `set_stats` renders the keys of its `data:` child, which is a *value*, so a top-level
/// walk never reaches them; it is listed anyway so the exemption is readable rather than
/// incidental to the walk's shape.
const RENDERS_KEYS: &[&str] = &["set_fact", "set_stats"];

/// Modules whose mapping args swallow an unknown key instead of naming it. Measured for
/// `command`; the rest share `_raw_params` handling. Skipped, because the diagnosis is
/// right but no message we could write would match the error the user actually gets.
const FREE_FORM: &[&str] =
    &["command", "shell", "raw", "script", "win_command", "win_shell", "meta"];

/// Takes arbitrary keys and keeps them, so the failure is silent rather than fatal.
const ADD_HOST: &str = "add_host";

#[derive(Debug, Clone)]
pub struct Problem {
    /// The offending key, braces included.
    pub span: Span,
    pub tier: Tier,
    pub message: String,
    pub rule: &'static str,
}

pub fn problems(ast: &Ast) -> Vec<Problem> {
    let mut out = Vec::new();
    match ast {
        Ast::Playbook(items) => {
            for item in items {
                let PlayItem::Play(p) = item else { continue };
                for s in p.pre_tasks.iter().chain(&p.tasks).chain(&p.post_tasks).chain(&p.handlers)
                {
                    stmt(s, &mut out);
                }
            }
        }
        Ast::Tasks(stmts) => stmts.iter().for_each(|s| stmt(s, &mut out)),
        Ast::Other => {}
    }
    out
}

fn stmt(s: &Stmt, out: &mut Vec<Problem>) {
    match s {
        Stmt::Task(t) => task(t, out),
        Stmt::Block(b) => {
            for inner in b.block.iter().chain(&b.rescue).chain(&b.always) {
                stmt(inner, out);
            }
        }
    }
}

fn task(t: &Task, out: &mut Vec<Problem>) {
    let Some(action) = t.action.as_ref() else { return };
    let name = keywords::core_action(&action.name);
    if RENDERS_KEYS.contains(&name) || FREE_FORM.contains(&name) {
        return;
    }
    // Only the mapping spelling has written arg keys; a free-form scalar is `_raw_params`.
    if !matches!(action.args, Node::Mapping { .. }) {
        return;
    }
    for (k, _) in action.args.entries() {
        let Some(key) = k.as_str() else { continue };
        if !key.contains("{{") {
            continue;
        }
        let (tier, message) = if name == ADD_HOST {
            (
                Tier::Warning,
                format!(
                    "`add_host` does not render argument keys, so this sets a host var \
                     literally named `{key}` — a name no expression can reference. The \
                     variable you meant is never set, and nothing fails. Set it through a \
                     rendered key instead: `vars: {{ \"{{{{ ... }}}}\": value }}` is not \
                     rendered either, so build the mapping with `set_fact` and pass it."
                ),
            )
        } else if name == "group_by" {
            (
                Tier::Error,
                format!("Invalid options for group_by: {key} — argument keys are never rendered"),
            )
        } else {
            (
                Tier::Error,
                format!(
                    "Unsupported parameters for ({}) module: {key} — argument keys are never \
                     rendered, so the module receives the braces as a literal parameter name \
                     and the task fails",
                    action.name
                ),
            )
        };
        out.push(Problem { span: k.span(), tier, message, rule: RULE_ID });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    fn problems_of(yaml: &str) -> Vec<Problem> {
        let nodes = Document::new(yaml.into()).parse().unwrap();
        problems(&crate::ast::build(&nodes))
    }

    fn one_task(action_block: &str) -> String {
        format!("- hosts: localhost\n  tasks:\n    - {action_block}\n")
    }

    /// The two fatal rows, each naming the error ansible-core actually reports — measured
    /// on 2.21.2 and deliberately different strings, because `group_by` validates its own
    /// options rather than going through the module arg spec.
    #[test]
    fn a_templated_key_on_an_ordinary_module_is_an_error_naming_the_real_failure() {
        let p = problems_of(&one_task("ansible.builtin.debug:\n        \"{{ argname }}\": x"));
        assert_eq!(p.len(), 1, "{p:?}");
        assert_eq!(p[0].tier, Tier::Error);
        assert!(
            p[0].message.starts_with("Unsupported parameters for (ansible.builtin.debug) module: {{ argname }}"),
            "{}",
            p[0].message
        );

        let g = problems_of(&one_task("group_by:\n        key: g1\n        \"{{ k }}\": v"));
        assert_eq!(g.len(), 1, "{g:?}");
        assert_eq!(g[0].tier, Tier::Error);
        assert!(
            g[0].message.starts_with("Invalid options for group_by: {{ k }}"),
            "{}",
            g[0].message
        );
    }

    /// The quiet one: `add_host` runs, and the host var is literally named `{{ k }}`.
    #[test]
    fn a_templated_key_on_add_host_is_a_warning_about_the_literal_name() {
        let p = problems_of(&one_task("add_host:\n        name: h1\n        \"{{ k }}\": v"));
        assert_eq!(p.len(), 1, "{p:?}");
        assert_eq!(p[0].tier, Tier::Warning, "add_host does not fail — it misbehaves");
        assert!(p[0].message.contains("literally named"), "{}", p[0].message);
    }

    /// Rule 2's control: the two spellings that are *correct* must stay silent, or the rule
    /// is flagging the one case where key templating is the documented feature.
    #[test]
    fn the_two_rendering_sites_stay_silent() {
        assert!(problems_of(&one_task("set_fact:\n        \"{{ k }}\": v")).is_empty());
        assert!(problems_of(&one_task("ansible.builtin.set_fact:\n        \"{{ k }}\": v")).is_empty());
        assert!(
            problems_of(&one_task("set_stats:\n        data:\n          \"{{ k }}\": 1")).is_empty()
        );
    }

    /// A key inside a value is ordinary data — legal, and meaningless. Asserted at two
    /// depths so the walk cannot start descending later without this failing.
    #[test]
    fn a_templated_key_nested_in_a_value_stays_silent_at_every_depth() {
        assert!(problems_of(&one_task("debug:\n        msg:\n          \"{{ k }}\": v")).is_empty());
        assert!(
            problems_of(&one_task(
                "debug:\n        msg:\n          outer:\n            deeper:\n              \"{{ k }}\": v"
            ))
            .is_empty()
        );
    }

    /// Free-form modules swallow the key and fail for another reason, so they are skipped
    /// rather than reported with a message that will not match the error.
    #[test]
    fn free_form_modules_are_skipped() {
        for m in ["command", "shell", "raw", "script"] {
            let p = problems_of(&one_task(&format!("{m}:\n        \"{{{{ k }}}}\": v")));
            assert!(p.is_empty(), "{m}: {p:?}");
        }
    }

    /// The controls that keep the rule from firing on everything: a key with no braces, and
    /// the free-form scalar spelling which has no written arg keys at all.
    #[test]
    fn an_ordinary_key_and_the_scalar_spelling_are_silent() {
        assert!(problems_of(&one_task("debug:\n        msg: hello")).is_empty());
        assert!(problems_of(&one_task("include_tasks: other.yml")).is_empty());
        assert!(problems_of(&one_task("debug: msg=hello")).is_empty());
    }

    /// Tasks inside a block are reached — a rule that only walked the top level would miss
    /// every task in a `block:`.
    #[test]
    fn a_templated_key_inside_a_block_is_reached() {
        let yaml = "- hosts: localhost\n  tasks:\n    - block:\n        - debug:\n            \"{{ k }}\": x\n";
        assert_eq!(problems_of(yaml).len(), 1);
    }

    /// T-170's corpus gate. Not run by default: the trees it needs are never committed.
    ///
    /// Prints every `templated-arg-key` hit with its file, line and tier, so the population
    /// the rule judges can be read rather than counted. A hit here is not automatically a
    /// bug — `add_host` with a templated key is a real fault worth reporting — but an
    /// *error*-tier hit on working production code would mean the rule is wrong, because
    /// that claims the task cannot run.
    #[test]
    #[ignore = "corpus gate: ANSIBLE_CORPUS=<path> cargo test -p ansible-core templated_key_corpus -- --ignored --nocapture"]
    fn templated_key_corpus() {
        let Ok(root) = std::env::var("ANSIBLE_CORPUS") else { return };
        let root = std::path::PathBuf::from(root);
        assert!(root.is_dir(), "ANSIBLE_CORPUS={} is not a directory", root.display());

        let files = crate::workspace::yaml_files(&root);
        assert!(!files.is_empty(), "no YAML under {}", root.display());
        let mut errors = 0usize;
        let mut warnings = 0usize;
        for path in &files {
            let Ok(text) = std::fs::read_to_string(path) else { continue };
            let Some(nodes) = Document::new(text.clone()).parse() else { continue };
            for p in problems(&crate::ast::build(&nodes)) {
                let line = text[..p.span.start.min(text.len())].lines().count();
                match p.tier {
                    Tier::Error => errors += 1,
                    Tier::Warning => warnings += 1,
                }
                println!("{:?} {}:{} {}", p.tier, path.display(), line, p.message);
            }
        }
        println!("--- {} files, {errors} error, {warnings} warning", files.len());
    }

}
