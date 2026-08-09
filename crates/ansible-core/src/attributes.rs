//! Unknown-key diagnostics (T-107): reproduce ansible-core's
//! `'%s' is not a valid attribute for a %s` (`base.py:211-220`) from the
//! [`crate::ast::UnknownKey`]s that [`crate::ast::build`] classified.

use crate::ast::{Ast, PlayItem, Stmt, Task, UnknownKey};
use crate::keywords::{self, KeyContext};
use crate::parse::Span;

/// Rule id, for `# noqa: invalid-attribute` and for display.
pub const RULE_ID: &str = "invalid-attribute";

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
                    });
                }
            }
        }
    }
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
    Problem {
        span: u.key_span,
        tier: Tier::Error,
        message: format!("'{}' is not a valid attribute for a {}", u.key, class),
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
