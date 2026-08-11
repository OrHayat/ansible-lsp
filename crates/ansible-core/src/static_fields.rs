//! T-103: a static field carrying a template is used literally.
//!
//! Four keywords are `static=True` upstream ([`keywords::STATIC_KEYWORDS`]) and are never
//! passed through the templar; `module_defaults` keys are equally literal through a no-op
//! post-validator (`task.py:178`). A `{{ }}` written in one of them is not a variable —
//! it is text, and what that costs was measured per field on ansible-core 2.21.2:
//!
//! - `register:` and a `vars:` mapping **key** — fatal at parse time (`Invalid variable
//!   name`), the playbook never runs. Measured at play, block, task and `roles:`-entry
//!   level for `vars`; `vars:` *values* template fine and are not this rule's business.
//! - a `module_defaults:` **key** — fatal before any task runs (`Could not resolve action
//!   ansible.legacy.{{ m }} in module_defaults`). Measured at play, block, task and
//!   `roles:`-entry level; *values* template when merged into args.
//! - a `collections` entry — the play runs; the entry matches nothing and plugin lookup
//!   silently falls through past it. Ansible warns at play and task level and says
//!   nothing at all on a `roles:` entry.
//! - `listen:` — the play runs; the braces stay in the topic name, so a `notify:` for the
//!   rendered name dies at run time with `handler not found`, blaming the topic rather
//!   than this line. Ansible warns only if the handler is *also* reachable by another
//!   name — in the failure mode that matters it reports nothing.
//!
//! Severity follows that split: error where Ansible refuses to run, warning where it runs
//! and misbehaves. The keys come from the keyword tables, not from this module: a fifth
//! static attribute added upstream lands in [`keywords::STATIC_KEYWORDS`] and fires here
//! with the generic message until its failure mode is measured.

use crate::keywords::{self, KeyContext};
use crate::parse::{Node, Span};

/// Rule id, for `# noqa: static-template` and for display.
pub const RULE_ID: &str = "static-template";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Problem {
    /// The template-carrying value or key.
    pub span: Span,
    pub tier: Tier,
    pub message: String,
    pub rule: &'static str,
}

const REGISTER: &str = "`register` is never templated — Ansible refuses this name at \
                        parse time (`Invalid variable name`) and the playbook does not \
                        run. Use a literal name.";
const VARS_KEY: &str = "a `vars:` key is never templated — Ansible refuses it at parse \
                        time (`Invalid variable name`) and the playbook does not run. \
                        Values may be templates; keys must be literal names.";
const MODULE_DEFAULTS_KEY: &str = "a `module_defaults:` key is never templated — Ansible \
                                   cannot resolve the braces as an action and fails \
                                   before any task runs. Values may be templates; the \
                                   module name must be literal.";
const COLLECTIONS_ENTRY: &str = "a `collections` entry is never templated — the braces \
                                 stay in the name, so this entry matches nothing and \
                                 plugin lookup silently falls through to the rest of the \
                                 list. The play still runs.";
const LISTEN: &str = "`listen` is never templated — the braces stay in the topic name, so \
                      a `notify:` for the rendered name can never reach this handler and \
                      fails at run time naming the topic, not this line. Ansible reports \
                      nothing here.";

/// Every static-field problem in the file. Playbooks are walked per play; a standalone
/// task file is walked with the handler vocabulary — the lenient superset, the same
/// choice [`keywords::is_task_directive`] makes, since `handlers/main.yml` reads exactly
/// like `tasks/main.yml`. A file with no top-level sequence (a vars file, `galaxy.yml`,
/// a mapping-form `requirements.yml` with its `collections:` key) is data, not keywords,
/// and is never entered — the same document selection `placement::problems` makes.
pub fn problems(nodes: &[Node]) -> Vec<Problem> {
    let mut out = Vec::new();
    let Some(seq) = nodes.iter().find(|n| matches!(n, Node::Sequence { .. })) else {
        return out;
    };
    let items = seq.items();
    let looks_like_plays = items
        .iter()
        .any(|it| keywords::is_play(it.entries().iter().filter_map(|(k, _)| k.as_str())));
    if !looks_like_plays {
        for item in items {
            stmt(item, KeyContext::Handler, &mut out);
        }
        return out;
    }
    for item in items {
        if matches!(item, Node::Mapping { .. }) {
            play(item, &mut out);
        }
    }
    out
}

fn play(node: &Node, out: &mut Vec<Problem>) {
    statics_on(node, KeyContext::Play, out);
    // A `roles:` entry carries the same Base attributes, and the fatal cases were
    // measured there too. Keys outside the RoleDefinition set are role params — values,
    // not keywords — and `legal_key` keeps this from misreading one named `register`.
    if let Some(roles) = node.get("roles") {
        for entry in roles.items() {
            if matches!(entry, Node::Mapping { .. }) {
                statics_on(entry, KeyContext::RoleDefinition, out);
            }
        }
    }
    for key in keywords::PLAY_TASK_CONTAINERS {
        let ctx = if *key == "handlers" { KeyContext::Handler } else { KeyContext::Task };
        if let Some(list) = node.get(key) {
            for item in list.items() {
                stmt(item, ctx, out);
            }
        }
    }
}

/// One entry of a task list. Blocks check their own keys against the Block vocabulary —
/// no `register`/`listen` there, so only the key-mapping fields apply — and recurse with
/// the surrounding context, which is what decides whether `listen` is legal on the tasks
/// inside.
fn stmt(node: &Node, ctx: KeyContext, out: &mut Vec<Problem>) {
    if !matches!(node, Node::Mapping { .. }) {
        return;
    }
    let is_block = keywords::BLOCK_TASK_CONTAINERS
        .iter()
        .any(|k| node.get(k).is_some());
    if is_block {
        statics_on(node, KeyContext::Block, out);
        for key in keywords::BLOCK_TASK_CONTAINERS {
            if let Some(list) = node.get(key) {
                for child in list.items() {
                    stmt(child, ctx, out);
                }
            }
        }
        return;
    }
    statics_on(node, ctx, out);
}

/// The check for one mapping: every static keyword present and legal in this context.
/// Illegal spellings (`register:` on a play, `listen:` on a task) are T-107's
/// invalid-attribute to report — a templating claim about a key Ansible refuses outright
/// would be a second, wrong story about the same line.
fn statics_on(node: &Node, ctx: KeyContext, out: &mut Vec<Problem>) {
    let names = keywords::STATIC_KEYWORDS.iter().copied().chain(["module_defaults"]);
    for key in names {
        if !keywords::legal_key(ctx, key) {
            continue;
        }
        // On a duplicate key Ansible warns and loads only the last — measured, a
        // discarded first `register: "{{ v }}"` runs clean while the same template
        // written last is fatal. So only the last occurrence is judged, the same read
        // `Node::get` takes.
        let Some((_, v)) = node.entries().iter().rev().find(|(k, _)| k.as_str() == Some(key))
        else {
            continue;
        };
        match key {
            // For these two it is the *keys* that are static while values template.
            "vars" => keys_of(v, VARS_KEY, out),
            "module_defaults" => keys_of(v, MODULE_DEFAULTS_KEY, out),
            _ => values_of(key, v, out),
        }
    }
}

/// Flag templated keys of a mapping-valued field. Only the top level: a nested mapping
/// under a `vars:` key is that variable's value, and `module_defaults` submaps are the
/// args, which template.
fn keys_of(value: &Node, message: &str, out: &mut Vec<Problem>) {
    if !matches!(value, Node::Mapping { .. }) {
        return;
    }
    for (k, _) in value.entries() {
        if k.as_str().is_some_and(|s| s.contains("{{")) {
            out.push(Problem {
                span: k.span(),
                tier: Tier::Error,
                message: message.into(),
                rule: RULE_ID,
            });
        }
    }
}

/// Flag templated scalar values — the field itself, or each entry of a list-valued one
/// (`listen` and `collections` both take lists). A mapping value is skipped: `register`
/// accepts a dict on 2.21 ("manual validation required", `task.py:89`) with semantics of
/// its own, and a claim about it is not measured.
fn values_of(key: &str, value: &Node, out: &mut Vec<Problem>) {
    let mut flag = |n: &Node| {
        if n.as_str().is_some_and(|s| s.contains("{{")) {
            let (tier, message) = verdict(key);
            out.push(Problem { span: n.span(), tier, message, rule: RULE_ID });
        }
    };
    match value {
        Node::Sequence { items, .. } => items.iter().for_each(&mut flag),
        scalar => flag(scalar),
    }
}

fn verdict(key: &str) -> (Tier, String) {
    match key {
        "register" => (Tier::Error, REGISTER.into()),
        "listen" => (Tier::Warning, LISTEN.into()),
        "collections" => (Tier::Warning, COLLECTIONS_ENTRY.into()),
        // A static attribute newer than the table above: flagged from day one, warning
        // tier until its failure mode is measured (T-103's severity split is per-field
        // measurement, and this field has none yet).
        other => (
            Tier::Warning,
            format!(
                "`{other}` is declared static in ansible-core — this template is never \
                 rendered and the braces are used literally."
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    fn check(src: &str) -> Vec<(Tier, String)> {
        let nodes = Document::new(src.to_string()).parse().expect("valid yaml");
        problems(&nodes).into_iter().map(|p| (p.tier, p.message)).collect()
    }

    fn spans(src: &str) -> Vec<String> {
        let nodes = Document::new(src.to_string()).parse().expect("valid yaml");
        problems(&nodes).into_iter().map(|p| p.span.slice(src).to_string()).collect()
    }

    #[test]
    fn a_templated_register_is_a_parse_time_error() {
        let got = check("- hosts: web\n  tasks:\n    - command: whoami\n      register: \"{{ v }}\"\n");
        assert_eq!(got, [(Tier::Error, REGISTER.to_string())]);
        // Partially templated is just as fatal — the name still carries braces.
        let got = check("- hosts: web\n  tasks:\n    - command: whoami\n      register: \"pre_{{ v }}\"\n");
        assert_eq!(got, [(Tier::Error, REGISTER.to_string())]);
        assert!(check("- hosts: web\n  tasks:\n    - command: whoami\n      register: out\n").is_empty());
    }

    /// The dict form of `register` is 2.21's own extension with semantics of its own —
    /// unmeasured, so unflagged.
    #[test]
    fn a_register_dict_is_not_judged() {
        assert!(check(
            "- hosts: web\n  tasks:\n    - command: whoami\n      register:\n        x: \"{{ v }}\"\n"
        )
        .is_empty());
    }

    /// `vars:` keys are static at every level the keyword is legal — measured at play,
    /// block, task and roles-entry level, all the same `Invalid variable name` fatal.
    #[test]
    fn templated_vars_keys_are_errors_everywhere() {
        for src in [
            "- hosts: web\n  vars:\n    \"{{ n }}\": 5\n  tasks: []\n",
            "- hosts: web\n  tasks:\n    - block:\n        - debug:\n      vars:\n        \"{{ n }}\": 5\n",
            "- hosts: web\n  tasks:\n    - debug:\n      vars:\n        \"{{ n }}\": 5\n",
            "- hosts: web\n  roles:\n    - role: r\n      vars:\n        \"{{ n }}\": 5\n",
            "- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      debug:\n      vars:\n        \"{{ n }}\": 5\n",
        ] {
            assert_eq!(check(src), [(Tier::Error, VARS_KEY.to_string())], "in: {src}");
        }
    }

    /// The other half of the `vars` rule: values template fine, so only the key may fire.
    #[test]
    fn templated_vars_values_are_not_flagged() {
        assert!(check("- hosts: web\n  vars:\n    a: \"{{ b }}\"\n  tasks: []\n").is_empty());
        assert!(check(
            "- hosts: web\n  tasks:\n    - debug:\n      vars:\n        a: \"{{ b }}\"\n"
        )
        .is_empty());
        // A nested mapping under a legal key is the variable's value, not more names.
        assert!(check(
            "- hosts: web\n  vars:\n    a:\n      \"{{ b }}\": 5\n  tasks: []\n"
        )
        .is_empty());
    }

    /// Measured fatal at play, block, task and roles-entry level alike.
    #[test]
    fn templated_module_defaults_keys_are_errors_everywhere() {
        for src in [
            "- hosts: web\n  module_defaults:\n    \"{{ m }}\":\n      msg: x\n  tasks: []\n",
            "- hosts: web\n  tasks:\n    - block:\n        - debug:\n      module_defaults:\n        \"{{ m }}\":\n          msg: x\n",
            "- hosts: web\n  tasks:\n    - debug:\n      module_defaults:\n        \"{{ m }}\":\n          msg: x\n",
            "- hosts: web\n  roles:\n    - role: r\n      module_defaults:\n        \"{{ m }}\":\n          msg: x\n",
        ] {
            assert_eq!(check(src), [(Tier::Error, MODULE_DEFAULTS_KEY.to_string())], "in: {src}");
        }
    }

    /// `module_defaults` values are merged into args and template there — measured.
    #[test]
    fn templated_module_defaults_values_are_not_flagged() {
        assert!(check(
            "- hosts: web\n  module_defaults:\n    debug:\n      msg: \"{{ x }}\"\n  tasks: []\n"
        )
        .is_empty());
    }

    #[test]
    fn templated_collections_entries_warn_per_entry() {
        let got = check(
            "- hosts: web\n  collections:\n    - \"{{ c }}\"\n    - community.general\n    - \"{{ d }}\"\n  tasks: []\n",
        );
        assert_eq!(
            got,
            [
                (Tier::Warning, COLLECTIONS_ENTRY.to_string()),
                (Tier::Warning, COLLECTIONS_ENTRY.to_string()),
            ]
        );
        // Task- and roles-entry-level are the same static attribute, measured dead there too.
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - debug:\n      collections: [\"{{ c }}\"]\n").len(),
            1
        );
        assert_eq!(
            check("- hosts: web\n  roles:\n    - role: r\n      collections: [\"{{ c }}\"]\n").len(),
            1
        );
    }

    #[test]
    fn templated_listen_warns_in_handlers_scalar_and_list() {
        let got = check(
            "- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      debug:\n      listen: \"{{ t }}\"\n",
        );
        assert_eq!(got, [(Tier::Warning, LISTEN.to_string())]);
        let got = check(
            "- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      debug:\n      listen:\n        - lit\n        - \"{{ t }}\"\n",
        );
        assert_eq!(got, [(Tier::Warning, LISTEN.to_string())]);
    }

    /// `listen` on a play's task is not a keyword — T-107's invalid-attribute owns that
    /// line, and a templating story about it would be a second, contradictory claim.
    #[test]
    fn listen_outside_handlers_is_not_ours() {
        assert!(check(
            "- hosts: web\n  tasks:\n    - debug:\n      listen: \"{{ t }}\"\n"
        )
        .is_empty());
    }

    /// A standalone task file could as well be `handlers/main.yml`, so the handler
    /// vocabulary applies — the same leniency `is_task_directive` takes.
    #[test]
    fn standalone_task_files_are_checked_with_the_handler_vocabulary() {
        let got = check("- command: whoami\n  register: \"{{ v }}\"\n");
        assert_eq!(got, [(Tier::Error, REGISTER.to_string())]);
        let got = check("- name: h\n  debug:\n  listen: \"{{ t }}\"\n");
        assert_eq!(got, [(Tier::Warning, LISTEN.to_string())]);
        // Blocks recurse; the block's own keys use the Block vocabulary.
        let got = check(
            "- block:\n    - command: whoami\n      register: \"{{ v }}\"\n  vars:\n    \"{{ n }}\": 5\n",
        );
        assert_eq!(
            got,
            [
                (Tier::Error, VARS_KEY.to_string()),
                (Tier::Error, REGISTER.to_string()),
            ]
        );
    }

    /// On a duplicate key Ansible warns `Using last defined value only` and loads just the
    /// last — measured, a discarded first `register: "{{ v }}"` runs clean while the same
    /// template written last is fatal. So only the last occurrence is judged.
    #[test]
    fn duplicate_keys_follow_ansibles_keep_last() {
        assert!(check(
            "- hosts: web\n  tasks:\n    - command: whoami\n      register: \"{{ v }}\"\n      register: ok\n"
        )
        .is_empty());
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - command: whoami\n      register: ok\n      register: \"{{ v }}\"\n"),
            [(Tier::Error, REGISTER.to_string())]
        );
        // The same read for the key-mapping fields: a discarded first `vars:` is not loaded.
        assert!(check(
            "- hosts: web\n  tasks:\n    - debug:\n      vars:\n        \"{{ n }}\": 5\n      vars:\n        ok: 1\n"
        )
        .is_empty());
    }

    /// The document guard: files whose top level is a mapping are data, and a key named
    /// like a keyword in them is not a keyword. `requirements.yml`'s `collections:` is
    /// the live example in the demo tree.
    #[test]
    fn mapping_documents_are_never_entered() {
        assert!(check("collections:\n  - \"{{ c }}\"\n").is_empty());
        assert!(check("register: \"{{ v }}\"\nvars:\n  \"{{ n }}\": 5\n").is_empty());
    }

    /// A templated value on a key that templates fine must never fire — the measured
    /// non-case table in T-103. `notify` and `loop` stand in for the lot.
    #[test]
    fn templating_keywords_stay_silent() {
        assert!(check(
            "- hosts: web\n  tasks:\n    - debug:\n      notify: \"{{ t }}\"\n      loop: \"{{ items }}\"\n  handlers:\n    - name: h\n      debug:\n",
        )
        .is_empty());
    }

    /// The diagnostic is anchored on the offending value or key, not the keyword.
    #[test]
    fn problems_anchor_on_the_template() {
        assert_eq!(
            spans("- hosts: web\n  tasks:\n    - command: whoami\n      register: \"{{ v }}\"\n"),
            ["{{ v }}"]
        );
        assert_eq!(
            spans("- hosts: web\n  vars:\n    \"{{ n }}\": 5\n  tasks: []\n"),
            ["{{ n }}"]
        );
    }
}
