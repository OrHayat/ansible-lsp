//! A semantic model of an Ansible file: plays, blocks, tasks, roles — ordered and nested,
//! each carrying its source [`Span`]. Built on top of the raw [`Node`] tree from
//! [`crate::parse`], using [`crate::keywords`] to tell a task's *module* from its
//! *directives*.
//!
//! This is a parallel layer. The existing resolvers ([`crate::references`],
//! [`crate::condition`]) still walk the raw tree; moving them onto this model is a later
//! step. Nothing here changes current behaviour — it only adds structure the variable and
//! execution features will need.
//!
//! Scope note: the module/args split is best-effort here (the first non-directive key).
//! Ansible's full `ModuleArgsParser` rules — `action:`/`local_action:`, free-form, `args:`
//! — are a follow-up; only the value-naming forms are handled below.

use crate::keywords;
use crate::parse::{Node, Span};

#[derive(Debug, Clone)]
pub enum Ast {
    /// A sequence of plays (and `import_playbook` entries).
    Playbook(Vec<PlayItem>),
    /// A task file: a flat, ordered list of tasks/blocks (`include_tasks` targets, role
    /// `tasks/*.yml`, handler files).
    Tasks(Vec<Stmt>),
    /// Neither — a vars file (top-level mapping), empty, or unrecognised.
    Other,
}

#[derive(Debug, Clone)]
pub enum PlayItem {
    Play(Play),
    /// `- import_playbook: other.yml` — a top-level entry, not a play.
    Import(Import),
}

#[derive(Debug, Clone)]
pub struct Import {
    /// Span of the `import_playbook` value.
    pub span: Span,
    pub file: Option<String>,
    pub directives: Vec<Directive>,
}

#[derive(Debug, Clone)]
pub struct Play {
    pub span: Span,
    pub name: Option<String>,
    /// Span of the `hosts:` value.
    pub hosts: Option<Span>,
    pub roles: Vec<RoleUse>,
    pub pre_tasks: Vec<Stmt>,
    pub tasks: Vec<Stmt>,
    pub post_tasks: Vec<Stmt>,
    pub handlers: Vec<Stmt>,
    /// Play-level directives other than the ones captured structurally above.
    pub directives: Vec<Directive>,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Task(Task),
    Block(Block),
}

#[derive(Debug, Clone)]
pub struct Block {
    pub span: Span,
    pub name: Option<String>,
    pub block: Vec<Stmt>,
    pub rescue: Vec<Stmt>,
    pub always: Vec<Stmt>,
    pub directives: Vec<Directive>,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub span: Span,
    pub name: Option<String>,
    /// The module and its args. `None` for a malformed task with no module key.
    pub action: Option<Action>,
    pub directives: Vec<Directive>,
}

#[derive(Debug, Clone)]
pub struct Action {
    /// The module name as written — short (`debug`) or FQCN (`ansible.builtin.debug`).
    pub name: String,
    /// Span of the module key (or of the `action:`/`local_action:` value).
    pub key_span: Span,
    /// Span of the args node.
    pub args: Span,
}

#[derive(Debug, Clone)]
pub struct RoleUse {
    pub name: String,
    pub span: Span,
}

/// An Ansible-owned key on a play/block/task, with the spans of its key and value.
#[derive(Debug, Clone)]
pub struct Directive {
    pub key: String,
    pub key_span: Span,
    pub value: Span,
}

/// Lift the raw document tree into the semantic model.
pub fn build(nodes: &[Node]) -> Ast {
    // An Ansible file is a single YAML document that is a sequence (multiple `---`
    // documents are rare); operate on the first sequence found.
    let Some(seq) = nodes.iter().find(|n| matches!(n, Node::Sequence { .. })) else {
        return Ast::Other;
    };
    let items = seq.items();
    let looks_like_plays = items
        .iter()
        .any(|it| keywords::is_play(it.entries().iter().filter_map(|(k, _)| k.as_str())));
    if looks_like_plays {
        Ast::Playbook(items.iter().filter_map(build_play_item).collect())
    } else {
        Ast::Tasks(build_stmts(items))
    }
}

fn short_key(key: &str) -> &str {
    key.rsplit('.').next().unwrap_or(key)
}

fn name_of(node: &Node) -> Option<String> {
    node.get("name").and_then(|n| n.as_str()).map(str::to_owned)
}

/// Collect the directive keys of `node` that `keep` accepts. Directives are never FQCN,
/// so any dotted key is skipped (it's the module).
fn collect_directives(node: &Node, keep: impl Fn(&str) -> bool) -> Vec<Directive> {
    node.entries()
        .iter()
        .filter_map(|(k, v)| {
            let key = k.as_str()?;
            if key.contains('.') || !keep(key) {
                return None;
            }
            Some(Directive {
                key: key.to_string(),
                key_span: k.span(),
                value: v.span(),
            })
        })
        .collect()
}

fn build_play_item(node: &Node) -> Option<PlayItem> {
    if !matches!(node, Node::Mapping { .. }) {
        return None;
    }
    if let Some(v) = node.get("import_playbook") {
        return Some(PlayItem::Import(Import {
            span: v.span(),
            file: v.as_str().map(str::to_owned),
            directives: collect_directives(node, keywords::is_play_directive),
        }));
    }
    Some(PlayItem::Play(build_play(node)))
}

fn build_play(node: &Node) -> Play {
    let stmts = |key: &str| {
        node.get(key)
            .map(|n| build_stmts(n.items()))
            .unwrap_or_default()
    };
    // Captured structurally, so not repeated in `directives`.
    const STRUCTURAL: &[&str] = &[
        "name",
        "hosts",
        "roles",
        "pre_tasks",
        "tasks",
        "post_tasks",
        "handlers",
    ];
    Play {
        span: node.span(),
        name: name_of(node),
        hosts: node.get("hosts").map(|n| n.span()),
        roles: node.get("roles").map(build_roles).unwrap_or_default(),
        pre_tasks: stmts("pre_tasks"),
        tasks: stmts("tasks"),
        post_tasks: stmts("post_tasks"),
        handlers: stmts("handlers"),
        directives: collect_directives(node, |k| {
            keywords::is_play_directive(k) && !STRUCTURAL.contains(&k)
        }),
    }
}

fn build_roles(roles: &Node) -> Vec<RoleUse> {
    roles
        .items()
        .iter()
        .filter_map(|item| match item {
            // - myrole
            Node::Scalar { value, span } => Some(RoleUse {
                name: value.clone(),
                span: *span,
            }),
            // - role: myrole
            Node::Mapping { .. } => match item.get("role") {
                Some(Node::Scalar { value, span }) => Some(RoleUse {
                    name: value.clone(),
                    span: *span,
                }),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn build_stmts(items: &[Node]) -> Vec<Stmt> {
    items.iter().filter_map(build_stmt).collect()
}

fn build_stmt(node: &Node) -> Option<Stmt> {
    if !matches!(node, Node::Mapping { .. }) {
        return None;
    }
    if node.get("block").is_some() {
        Some(Stmt::Block(build_block(node)))
    } else {
        Some(Stmt::Task(build_task(node)))
    }
}

fn build_block(node: &Node) -> Block {
    let stmts = |key: &str| {
        node.get(key)
            .map(|n| build_stmts(n.items()))
            .unwrap_or_default()
    };
    Block {
        span: node.span(),
        name: name_of(node),
        block: stmts("block"),
        rescue: stmts("rescue"),
        always: stmts("always"),
        directives: collect_directives(node, |k| {
            keywords::is_block_directive(k)
                && k != "name"
                && !keywords::BLOCK_TASK_CONTAINERS.contains(&k)
        }),
    }
}

fn build_task(node: &Node) -> Task {
    Task {
        span: node.span(),
        name: name_of(node),
        action: find_action(node),
        directives: collect_directives(node, |k| keywords::is_task_directive(k) && k != "name"),
    }
}

/// The module on a task: the value of `action:`/`local_action:`, else the first key that
/// isn't a directive (a bare non-directive, or any FQCN).
fn find_action(node: &Node) -> Option<Action> {
    for holder in ["action", "local_action"] {
        if let Some(v) = node.get(holder) {
            if let Some(s) = v.as_str() {
                let name = s.split_whitespace().next().unwrap_or(s).to_string();
                return Some(Action {
                    name,
                    key_span: v.span(),
                    args: v.span(),
                });
            }
        }
    }
    for (k, v) in node.entries() {
        let Some(key) = k.as_str() else { continue };
        let is_directive = !key.contains('.') && keywords::is_task_directive(short_key(key));
        if !is_directive {
            return Some(Action {
                name: key.to_string(),
                key_span: k.span(),
                args: v.span(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    fn ast(src: &str) -> Ast {
        build(&Document::new(src.to_string()).parse().expect("valid yaml"))
    }

    #[test]
    fn playbook_with_plays_and_import() {
        let a = ast(
            "- name: web\n  hosts: web\n  roles: [nginx, {role: certs}]\n  tasks:\n    - debug: {msg: hi}\n\
             - import_playbook: other.yml\n",
        );
        let Ast::Playbook(items) = a else {
            panic!("expected a playbook, got {a:?}");
        };
        assert_eq!(items.len(), 2);
        let PlayItem::Play(play) = &items[0] else {
            panic!("first item should be a play");
        };
        assert_eq!(play.name.as_deref(), Some("web"));
        assert!(play.hosts.is_some());
        assert_eq!(play.roles.len(), 2);
        assert_eq!(play.roles[0].name, "nginx");
        assert_eq!(play.roles[1].name, "certs");
        assert_eq!(play.tasks.len(), 1);
        let Stmt::Task(t) = &play.tasks[0] else {
            panic!("expected a task");
        };
        assert_eq!(t.action.as_ref().unwrap().name, "debug");

        let PlayItem::Import(imp) = &items[1] else {
            panic!("second item should be an import");
        };
        assert_eq!(imp.file.as_deref(), Some("other.yml"));
    }

    #[test]
    fn task_file_is_a_flat_list() {
        let a = ast("- name: one\n  command: echo hi\n- ansible.builtin.debug:\n    msg: two\n");
        let Ast::Tasks(stmts) = a else {
            panic!("expected a task list, got {a:?}");
        };
        assert_eq!(stmts.len(), 2);
        let Stmt::Task(t0) = &stmts[0] else { panic!() };
        assert_eq!(t0.name.as_deref(), Some("one"));
        assert_eq!(t0.action.as_ref().unwrap().name, "command");
        let Stmt::Task(t1) = &stmts[1] else { panic!() };
        assert_eq!(t1.action.as_ref().unwrap().name, "ansible.builtin.debug");
    }

    #[test]
    fn module_is_separated_from_directives() {
        let a = ast("- name: t\n  when: x is defined\n  loop: [1, 2]\n  register: out\n  command: echo hi\n");
        let Ast::Tasks(stmts) = a else { panic!() };
        let Stmt::Task(t) = &stmts[0] else { panic!() };
        assert_eq!(t.action.as_ref().unwrap().name, "command");
        // when/loop/register are directives; name is captured separately.
        let keys: Vec<&str> = t.directives.iter().map(|d| d.key.as_str()).collect();
        assert!(keys.contains(&"when"));
        assert!(keys.contains(&"loop"));
        assert!(keys.contains(&"register"));
        assert!(!keys.contains(&"command"), "command is the module, not a directive");
        assert!(!keys.contains(&"name"));
    }

    #[test]
    fn block_with_rescue_and_always() {
        let a = ast(
            "- block:\n    - debug: {msg: try}\n  rescue:\n    - debug: {msg: catch}\n  always:\n    - debug: {msg: fin}\n  when: risky\n",
        );
        let Ast::Tasks(stmts) = a else { panic!() };
        let Stmt::Block(b) = &stmts[0] else {
            panic!("expected a block");
        };
        assert_eq!(b.block.len(), 1);
        assert_eq!(b.rescue.len(), 1);
        assert_eq!(b.always.len(), 1);
        assert!(b.directives.iter().any(|d| d.key == "when"));
    }

    #[test]
    fn action_form_names_the_module() {
        let a = ast("- name: legacy\n  action: command echo hi\n");
        let Ast::Tasks(stmts) = a else { panic!() };
        let Stmt::Task(t) = &stmts[0] else { panic!() };
        assert_eq!(t.action.as_ref().unwrap().name, "command");
    }

    #[test]
    fn vars_file_is_other() {
        assert!(matches!(ast("key: value\nother: 2\n"), Ast::Other));
    }
}
