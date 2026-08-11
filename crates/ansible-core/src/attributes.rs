//! Unknown-key diagnostics (T-107): reproduce ansible-core's
//! `'%s' is not a valid attribute for a %s` (`base.py:211-220`) from the
//! [`crate::ast::UnknownKey`]s that [`crate::ast::build`] classified.

use crate::ast::{Ast, PlayItem, RoleUse, Stmt, Task, UnknownKey};
use crate::keywords::{self, KeyContext};
use crate::parse::{Node, Span};

/// Rule id, for `# noqa: invalid-attribute` and for display.
pub const RULE_ID: &str = "invalid-attribute";

/// T-100, and ours alone: on a `roles:` entry an unrecognised key is not an error but a
/// role param — a variable — and ansible-core is silent by design, at load *and* at run
/// time (`definition.py:207-222`, `role/__init__.py:552-555` calls them "inline variables
/// in role invocation"). The identical typo on a task is fatal. Separate id because this
/// replicates no upstream message and, unlike everything else in this module, fires on
/// code that ansible-core accepts.
pub const ROLE_PARAM_RULE_ID: &str = "role-param-not-keyword";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Problem {
    /// The offending key.
    pub span: Span,
    pub tier: Tier,
    pub message: String,
    /// Which rule fired — [`RULE_ID`] for everything that replicates ansible-core,
    /// [`ROLE_PARAM_RULE_ID`] for the one that speaks where ansible-core is silent.
    pub rule: &'static str,
}

/// Every unknown-key problem in the file. `invalid_task_attribute_failed` is the config
/// setting of that name (`base.yml:1703-1713`): true — Ansible's default — makes
/// task-level unknowns the load error, false downgrades them to the
/// `Ignoring invalid attribute` warning (`task.py:339-342`, `task_include.py:94-97`).
/// Play-, block- and `loop_control`-level unknowns are fatal regardless: their classes
/// run the plain `_validate_attributes` with no escape.
pub fn problems(ast: &Ast, invalid_task_attribute_failed: bool) -> Vec<Problem> {
    let mut out = Vec::new();
    match ast {
        Ast::Playbook(items) => {
            for item in items {
                if let PlayItem::Play(p) = item {
                    out.extend(p.unknown_keys.iter().map(|u| fatal(u, "Play")));
                    for r in &p.roles {
                        role_param_problems(r, &mut out);
                    }
                    for s in p.pre_tasks.iter().chain(&p.tasks).chain(&p.post_tasks) {
                        stmt(s, false, invalid_task_attribute_failed, &mut out);
                    }
                    for s in &p.handlers {
                        stmt(s, true, invalid_task_attribute_failed, &mut out);
                    }
                }
            }
        }
        // A standalone file may be a role's tasks/ or handlers/ — `ast::build` checked
        // the handler superset, so anything flagged is invalid in both contexts; the
        // class in the message is named for the common case.
        Ast::Tasks(stmts) => {
            for s in stmts {
                stmt(s, false, invalid_task_attribute_failed, &mut out);
            }
        }
        Ast::Other => {}
    }
    out
}

fn stmt(s: &Stmt, in_handlers: bool, failed: bool, out: &mut Vec<Problem>) {
    match s {
        Stmt::Block(b) => {
            out.extend(b.unknown_keys.iter().map(|u| fatal(u, "Block")));
            for child in b.block.iter().chain(&b.rescue).chain(&b.always) {
                stmt(child, in_handlers, failed, out);
            }
        }
        Stmt::Task(t) => {
            include_args_problems(t, out);
            for u in &t.unknown_keys {
                if u.ctx == KeyContext::LoopControl {
                    // `LoopControl.load` runs `_validate_attributes` itself, before the
                    // task-level severity split can intervene (`task.py:346-354`).
                    out.push(fatal(u, "LoopControl"));
                } else if failed {
                    out.push(fatal(u, class_name(t, u, in_handlers)));
                } else {
                    out.push(Problem {
                        span: u.key_span,
                        tier: Tier::Warning,
                        message: format!("Ignoring invalid attribute: {}", u.key),
                        rule: RULE_ID,
                    });
                }
            }
        }
    }
}

/// T-100: role params that name a keyword. Every param here is a legal, silent variable
/// definition, so most must stay quiet — passing parameters this way is the documented
/// idiom and a bare `port_count: 4` is not a mistake. Only two shapes are reported, both
/// on the same evidence: the author wrote something that *is* a keyword somewhere in a
/// playbook, and here it is a variable instead.
fn role_param_problems(r: &RoleUse, out: &mut Vec<Problem>) {
    for p in &r.params {
        let Some(why) = param_suspicion(&p.name) else { continue };
        out.push(Problem {
            span: p.key_span,
            tier: Tier::Warning,
            message: format!(
                "'{}' is not a keyword on a roles: entry — it defines a variable for role \
                 '{}' instead ({why})",
                p.name, r.name
            ),
            rule: ROLE_PARAM_RULE_ID,
        });
    }
}

/// Why a role param is worth a word, or `None` to stay silent. Both arms are derived from
/// the keyword tables rather than a hand-written list, so a keyword upstream adds is
/// covered without an edit here.
fn param_suspicion(key: &str) -> Option<String> {
    // A one-edit neighbour is the stronger signal, and it names the repair, so it wins
    // when a key is both (`becom_user` is only a near-miss; `tasks_from` is only a
    // keyword elsewhere).
    if let Some(s) = suggestion(key, KeyContext::RoleDefinition) {
        return Some(format!("did you mean '{s}'?"));
    }
    // The `roles:` legal set is already excluded upstream of this call, so anything left
    // that a play, a task or an `include_role` would accept was written in the wrong
    // place — most often `tasks_from:`, which does select an entry point on
    // `include_role:` and does nothing at all here.
    if keywords::ROLE_INCLUDE_KEYS.contains(&key) {
        return Some(format!("'{key}' selects this only on include_role:"));
    }
    if keywords::legal_key(KeyContext::Task, key) || keywords::legal_key(KeyContext::Play, key) {
        return Some(format!("'{key}' is a keyword on a task or play, not here"));
    }
    None
}

/// The include/import actions carry closed *args* sets on top of the keyword rules
/// (T-101): unknown args are `Invalid options` (`task_include.py:70-72`,
/// `role_include.py:137-139`); `apply` — and `rescuable` for roles — are include-only,
/// fatal on the import twins (`task_include.py:79-81`, `role_include.py:150-159`); and
/// a legal `apply:`'s contents load as a Block at expansion time (`task_include.py:
/// 106-124`), so a bad key inside it is a runtime error even `--syntax-check` misses.
/// All unconditional raises — `INVALID_TASK_ATTRIBUTE_FAILED` never softens these.
fn include_args_problems(t: &Task, out: &mut Vec<Problem>) {
    let Some(action) = t.action.as_ref() else { return };
    let (valid, is_import): (&[&str], bool) = match keywords::core_action(&action.name) {
        "include_tasks" => (keywords::TASK_INCLUDE_ARGS, false),
        "import_tasks" => (keywords::TASK_INCLUDE_ARGS, true),
        "include_role" => (keywords::ROLE_INCLUDE_KEYS, false),
        "import_role" => (keywords::ROLE_INCLUDE_KEYS, true),
        _ => return,
    };
    // Only the mapping spelling has written arg keys; free-form (`include_tasks: x.yml`)
    // is `_raw_params` and has nothing to check.
    if !matches!(action.args, Node::Mapping { .. }) {
        return;
    }
    for (k, v) in action.args.entries() {
        let Some(key) = k.as_str() else { continue };
        let invalid_option = |out: &mut Vec<Problem>| {
            out.push(Problem {
                span: k.span(),
                tier: Tier::Error,
                message: format!("Invalid options for {}: {}", action.name, key),
                rule: RULE_ID,
            });
        };
        if !valid.contains(&key) {
            invalid_option(out);
        } else if is_import && (key == "apply" || key == "rescuable") {
            // The raise sites test Python truthiness (`if apply_attrs and ...`,
            // `task_include.py:80`, `role_include.py:152,158`) — live-verified:
            // `rescuable: false` and `apply: {}` on an import run clean, while any
            // truthy value is `Invalid options`. Falsy-but-wrong-typed `apply` gets a
            // different error (`Expected a dict`), which is type checking, not ours.
            if !yaml_falsy(v) {
                invalid_option(out);
            }
        } else if key == "apply" {
            for u in unknown_block_keys(v) {
                out.push(fatal(&u, "Block"));
            }
        }
    }
}

/// Would this YAML value be falsy once Ansible's loader turns it into Python? Empty
/// collections, null spellings, the YAML-1.1 false spellings the loader resolves to
/// `bool` (single-letter `y`/`n` are strings, not bools), and integer zero.
fn yaml_falsy(n: &Node) -> bool {
    match n {
        Node::Scalar { value, .. } => matches!(
            value.as_str(),
            "" | "~"
                | "null" | "Null" | "NULL"
                | "0"
                | "no" | "No" | "NO"
                | "false" | "False" | "FALSE"
                | "off" | "Off" | "OFF"
        ),
        _ => n.entries().is_empty() && n.items().is_empty(),
    }
}

/// Keys of an `apply:` mapping that `Block.load` would reject at expansion time.
fn unknown_block_keys(apply: &Node) -> Vec<UnknownKey> {
    apply
        .entries()
        .iter()
        .filter_map(|(k, _)| {
            let key = k.as_str()?;
            if keywords::legal_key(KeyContext::Block, key) {
                return None;
            }
            Some(UnknownKey { key: key.to_string(), key_span: k.span(), ctx: KeyContext::Block })
        })
        .collect()
}

/// The class Ansible would name in the error — `self.__class__.__name__` at the raise
/// site, which depends on the action and the container, not just the key.
fn class_name(t: &Task, u: &UnknownKey, in_handlers: bool) -> &'static str {
    match u.ctx {
        KeyContext::DynamicInclude | KeyContext::DynamicHandlerInclude => {
            let action = t.action.as_ref().map(|a| keywords::core_action(&a.name));
            match (action, in_handlers) {
                (Some("include_role"), _) => "IncludeRole",
                (_, true) => "HandlerTaskInclude",
                _ => "TaskInclude",
            }
        }
        _ if in_handlers => "Handler",
        _ => "Task",
    }
}

fn fatal(u: &UnknownKey, class: &str) -> Problem {
    let hint = match suggestion(&u.key, u.ctx) {
        Some(s) => format!(" (did you mean '{s}'?)"),
        None => String::new(),
    };
    Problem {
        span: u.key_span,
        tier: Tier::Error,
        message: format!("'{}' is not a valid attribute for a {}{}", u.key, class, hint),
        rule: RULE_ID,
    }
}

/// A legal key of `ctx` one edit away from the typo — `vars_file` → `vars_files`,
/// `task` → `tasks` (T-088). One edit only: with a ~40-word dictionary, distance 2 starts
/// pairing unrelated keywords.
fn suggestion(key: &str, ctx: KeyContext) -> Option<&'static str> {
    keywords::legal_keys(ctx).find(|k| one_edit_apart(key, k))
}

/// Levenshtein distance 1 (plus adjacent transposition), without building the matrix:
/// walk to the first mismatch, then require the tails to line up under exactly one of
/// skip-a-char / drop-a-char / swap-the-pair.
fn one_edit_apart(a: &str, b: &str) -> bool {
    if a == b {
        return false;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let i = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let (ta, tb) = (&a[i..], &b[i..]);
    match (ta.len(), tb.len()) {
        (0, 1) | (1, 0) => true,                                  // insert / delete at end
        (x, y) if x == y => {
            ta[1..] == tb[1..]                                    // substitution
                || (ta.len() >= 2 && ta[0] == tb[1] && ta[1] == tb[0] && ta[2..] == tb[2..])
        }
        (x, y) if x + 1 == y => ta[..] == tb[1..],                // insertion
        (x, y) if x == y + 1 => ta[1..] == tb[..],                // deletion
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast;
    use crate::parse::Document;

    fn check(src: &str, failed: bool) -> Vec<(Tier, String)> {
        let nodes = Document::new(src.to_string()).parse().expect("valid yaml");
        problems(&ast::build(&nodes), failed)
            .into_iter()
            .map(|p| (p.tier, p.message))
            .collect()
    }

    #[test]
    fn play_level_unknowns_are_always_errors() {
        let got = check("- hosts: web\n  when: x\n  tasks:\n    - debug:\n", false);
        assert_eq!(
            got,
            [(Tier::Error, "'when' is not a valid attribute for a Play".into())]
        );
    }

    #[test]
    fn loop_on_a_block_is_an_error() {
        let got = check(
            "- hosts: web\n  tasks:\n    - block:\n        - debug:\n      loop: [1]\n",
            true,
        );
        assert_eq!(
            got,
            [(Tier::Error, "'loop' is not a valid attribute for a Block".into())]
        );
    }

    #[test]
    fn task_severity_follows_invalid_task_attribute_failed() {
        let src = "- hosts: web\n  tasks:\n    - debug:\n      listen: x\n";
        assert_eq!(
            check(src, true),
            [(Tier::Error, "'listen' is not a valid attribute for a Task".into())]
        );
        assert_eq!(
            check(src, false),
            [(Tier::Warning, "Ignoring invalid attribute: listen".into())]
        );
    }

    #[test]
    fn the_class_in_the_message_matches_ansibles() {
        let got = check(
            "- hosts: web\n  tasks:\n    - include_tasks: f.yml\n      become: true\n    \
             - include_role: {name: r}\n      become: true\n  handlers:\n    - debug:\n      \
             hosts: web\n",
            true,
        );
        assert_eq!(
            got,
            [
                (Tier::Error, "'become' is not a valid attribute for a TaskInclude".into()),
                (Tier::Error, "'become' is not a valid attribute for a IncludeRole".into()),
                (Tier::Error, "'hosts' is not a valid attribute for a Handler".into()),
            ]
        );
    }

    #[test]
    fn loop_control_unknowns_ignore_the_severity_config() {
        let got = check(
            "- hosts: web\n  tasks:\n    - debug:\n      loop: [1]\n      loop_control:\n        \
             name: x\n",
            false,
        );
        assert_eq!(
            got,
            [(Tier::Error, "'name' is not a valid attribute for a LoopControl".into())]
        );
    }

    /// T-088: the live-verified typo that filed the ticket, plus the no-hint case.
    #[test]
    fn a_one_edit_typo_gets_a_suggestion() {
        let got = check("- hosts: web\n  vars_file: x.yml\n  tasks:\n    - debug:\n", true);
        assert_eq!(
            got,
            [(
                Tier::Error,
                "'vars_file' is not a valid attribute for a Play (did you mean 'vars_files'?)"
                    .into()
            )]
        );
        let got = check("- hosts: web\n  frobnicate: x\n  tasks:\n    - debug:\n", true);
        assert_eq!(
            got,
            [(Tier::Error, "'frobnicate' is not a valid attribute for a Play".into())]
        );
    }

    /// T-101: `apply:` is include-only; on the import twins it is `Invalid options`.
    #[test]
    fn apply_on_an_import_is_an_error() {
        let got = check(
            "- hosts: web\n  tasks:\n    - import_tasks: {file: f.yml, apply: {become: true}}\n    \
             - import_role: {name: r, apply: {become: true}}\n    \
             - import_role: {name: r, rescuable: true}\n",
            true,
        );
        assert_eq!(
            got,
            [
                (Tier::Error, "Invalid options for import_tasks: apply".into()),
                (Tier::Error, "Invalid options for import_role: apply".into()),
                (Tier::Error, "Invalid options for import_role: rescuable".into()),
            ]
        );
    }

    /// The raise sites test truthiness, so falsy values pass — live-verified:
    /// `rescuable: false` and `apply: {}` on imports run clean in Ansible.
    #[test]
    fn falsy_apply_and_rescuable_on_imports_stay_silent() {
        let got = check(
            "- hosts: web\n  tasks:\n    - import_tasks: {file: f.yml, apply: {}}\n    \
             - import_role: {name: r, rescuable: false}\n    \
             - import_role: {name: r, rescuable: no}\n",
            true,
        );
        assert!(got.is_empty(), "found: {got:?}");
        // Known miss, not a lie: a QUOTED "no" is a truthy string and Ansible errors on
        // it, but scalar style isn't distinguishable here, so it passes silently.
    }

    /// T-101: a legal `apply:` has its contents checked as the Block it becomes at
    /// expansion time — an error `--syntax-check` never reaches.
    #[test]
    fn apply_contents_are_checked_as_a_block() {
        let ok = check(
            "- hosts: web\n  tasks:\n    - include_tasks: {file: f.yml, apply: {become: true, tags: [x]}}\n    \
             - include_role: {name: r, apply: {when: y}, rescuable: false}\n",
            true,
        );
        assert!(ok.is_empty(), "found: {ok:?}");
        let got = check(
            "- hosts: web\n  tasks:\n    - include_tasks: {file: f.yml, apply: {retries: 3}}\n",
            true,
        );
        assert_eq!(
            got,
            [(Tier::Error, "'retries' is not a valid attribute for a Block".into())]
        );
    }

    /// T-101: the include arg sets are closed — unknown args error even on includes.
    #[test]
    fn unknown_include_args_are_invalid_options() {
        let got = check(
            "- hosts: web\n  tasks:\n    - include_tasks: {file: f.yml, name: x}\n    \
             - include_role: {name: r, frobnicate: 1}\n",
            true,
        );
        assert_eq!(
            got,
            [
                (Tier::Error, "Invalid options for include_tasks: name".into()),
                (Tier::Error, "Invalid options for include_role: frobnicate".into()),
            ]
        );
        // The free-form spelling has no written arg keys — nothing to check.
        assert!(check("- hosts: web\n  tasks:\n    - include_tasks: f.yml\n", true).is_empty());
    }

    /// T-100, the ticket's own repro. Both lines read as settings and are variables;
    /// ansible-core reports neither, at load or at run time.
    #[test]
    fn keyword_shaped_role_params_warn() {
        let got = check(
            "- hosts: web\n  roles:\n    - role: web\n      tasks_from: alternate.yml\n      \
             becom_user: root\n",
            true,
        );
        assert_eq!(
            got,
            [
                (
                    Tier::Warning,
                    "'tasks_from' is not a keyword on a roles: entry — it defines a variable \
                     for role 'web' instead ('tasks_from' selects this only on include_role:)"
                        .into()
                ),
                (
                    Tier::Warning,
                    "'becom_user' is not a keyword on a roles: entry — it defines a variable \
                     for role 'web' instead (did you mean 'become_user'?)"
                        .into()
                ),
            ]
        );
    }

    /// The other half of the trade: passing parameters this way is the documented idiom
    /// (`role/__init__.py:552-555`), so a param that names no keyword must stay silent —
    /// this rule is worthless the moment it fires on `port_count: 4`.
    #[test]
    fn ordinary_role_params_stay_silent() {
        let got = check(
            "- hosts: web\n  roles:\n    - role: web\n      port_count: 4\n      \
             lustre_mount: /mnt\n      state: present\n",
            true,
        );
        assert!(got.is_empty(), "found: {got:?}");
    }

    /// Every key of `RoleInclude.fattributes` is a real setting here, not a param — a
    /// warning on any of them would be a false positive on code that works.
    #[test]
    fn role_definition_keywords_are_not_params() {
        let got = check(
            "- hosts: web\n  roles:\n    - role: web\n      when: x\n      tags: [t]\n      \
             become: true\n      become_user: root\n      vars: {a: 1}\n      \
             delegate_to: h\n      collections: [c.d]\n      name: label\n",
            true,
        );
        assert!(got.is_empty(), "found: {got:?}");
    }

    /// The bare-string form has no mapping to split, so it can carry no params.
    #[test]
    fn a_bare_string_role_has_no_params() {
        assert!(check("- hosts: web\n  roles:\n    - web\n", true).is_empty());
    }

    /// The rule takes its own id: it fires on code ansible-core accepts, so it must be
    /// suppressible and toggleable without silencing the replication rules.
    #[test]
    fn role_params_carry_their_own_rule_id() {
        let nodes = Document::new(
            "- hosts: web\n  roles:\n    - role: web\n      tasks_from: x\n      nmae: y\n"
                .to_string(),
        )
        .parse()
        .expect("valid yaml");
        let got = problems(&ast::build(&nodes), true);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().all(|p| p.rule == ROLE_PARAM_RULE_ID), "{got:?}");
    }

    /// T-063: the closed set of 11 (`role_include.py:40-43`) in full. Every one of these
    /// is legal and most do nothing statically — but the set being closed means a
    /// *stricter* list here turns each into a false `Invalid options` on a working play,
    /// which is why the boring ones are worth a test at all.
    #[test]
    fn every_valid_include_role_arg_stays_silent() {
        for arg in [
            "name: r", "role: r", "tasks_from: t", "vars_from: v", "defaults_from: d",
            "handlers_from: h", "public: true", "allow_duplicates: false",
            "rolespec_validate: false", "rescuable: false", "apply: {tags: [x]}",
        ] {
            let src = format!("- hosts: web\n  tasks:\n    - include_role: {{name: r, {arg}}}\n");
            assert!(check(&src, true).is_empty(), "{arg} should be a valid include_role arg");
        }
    }

    /// The import twins take the same set minus two. `public`, `allow_duplicates` and
    /// `rolespec_validate` are **not** among the rejected — measured, only `apply` and
    /// `rescuable` raise (`role_include.py:150-159`), so flagging a third would be a
    /// false error.
    #[test]
    fn only_apply_and_rescuable_are_include_only() {
        for arg in ["public: true", "allow_duplicates: false", "rolespec_validate: false",
                    "vars_from: v", "defaults_from: d", "handlers_from: h"] {
            let src = format!("- hosts: web\n  tasks:\n    - import_role: {{name: r, {arg}}}\n");
            assert!(check(&src, true).is_empty(), "{arg} is legal on import_role");
        }
        let got = check(
            "- hosts: web\n  tasks:\n    - import_role: {name: r, apply: {tags: [x]}}\n    \
             - import_role: {name: r, rescuable: true}\n",
            true,
        );
        assert_eq!(
            got,
            [
                (Tier::Error, "Invalid options for import_role: apply".into()),
                (Tier::Error, "Invalid options for import_role: rescuable".into()),
            ]
        );
    }

    #[test]
    fn a_clean_playbook_has_no_problems() {
        let got = check(
            "- hosts: web\n  become: true\n  tasks:\n    - community.general.ufw: {rule: allow}\n      \
             when: x\n      with_items: [a]\n",
            true,
        );
        assert!(got.is_empty(), "found: {got:?}");
    }
}
