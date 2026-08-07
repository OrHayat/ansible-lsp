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
    /// Literal `name: value` pairs from a `vars:` written on the import entry itself.
    /// Only scalars — they are the only substitutable form. This is one of exactly two
    /// sources that can supply a templated `import_playbook` (the other is `-e`), because
    /// the import is expanded at parse time from `self.vars | variable_manager.get_vars()`
    /// with no play, host or task (`playbook_include.py:69-83`). T-095.
    pub vars: Vec<(String, String)>,
    /// A `when:` on a static import is copied onto every imported task — see
    /// [`crate::condition`]. ANDed clauses; empty if absent.
    pub when: Vec<String>,
    pub when_span: Option<Span>,
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
    /// `vars:` bound at play scope.
    pub vars: Vec<VarBinding>,
    /// `vars_files:` entries, one per written entry — a nested list stays one entry
    /// with several first-match alternatives.
    pub vars_files: Vec<VarsFilesEntry>,
    /// Play-level directives other than the ones captured structurally above.
    pub directives: Vec<Directive>,
}

/// One `vars_files:` entry: a scalar path, or a nested list meaning "load the first of
/// these that exists".
#[derive(Debug, Clone)]
pub struct VarsFilesEntry {
    /// The candidate paths in written order; a scalar entry is one alternative.
    pub alternatives: Vec<(String, Span)>,
    /// Anchor for a whole-entry diagnostic: the scalar's span, or the nested list's span
    /// clamped to the last alternative's end (libyaml's block-sequence end mark can spill
    /// past the last item).
    pub span: Span,
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
    /// `when:` clauses on the block. ANDed; empty if absent.
    pub when: Vec<String>,
    pub when_span: Option<Span>,
    /// `vars:` bound at block scope.
    pub vars: Vec<VarBinding>,
    pub directives: Vec<Directive>,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub span: Span,
    pub name: Option<String>,
    /// The module and its args. `None` for a malformed task with no module key.
    pub action: Option<Action>,
    /// `when:` clauses. ANDed together; empty if the task is unconditional.
    pub when: Vec<String>,
    pub when_span: Option<Span>,
    /// The task has a `loop:`/`with_*`, so it may run many times.
    pub looped: bool,
    /// `register:` name — a variable this task defines for the rest of the play.
    pub register: Option<String>,
    /// Span of the `register:` value, if present.
    pub register_span: Option<Span>,
    /// `vars:` bound at task scope.
    pub vars: Vec<VarBinding>,
    pub directives: Vec<Directive>,
}

#[derive(Debug, Clone)]
pub struct Action {
    /// The module name as written — short (`debug`) or FQCN (`ansible.builtin.debug`).
    pub name: String,
    /// Span of the module key (or of the `action:`/`local_action:` value).
    pub key_span: Span,
    /// The args node: a scalar (`include_tasks: f.yml`) or a mapping (`{ file: f.yml }` /
    /// module parameters). Kept whole so callers can read `file:`/`name:`/`tasks_from:`.
    pub args: Node,
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

/// A `name: value` entry under a `vars:` mapping. The span covers the value, to anchor
/// go-to-definition on the variable.
#[derive(Debug, Clone)]
pub struct VarBinding {
    pub name: String,
    pub span: Span,
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

fn name_of(node: &Node) -> Option<String> {
    node.get("name").and_then(|n| n.as_str()).map(str::to_owned)
}

/// A `when:` is either one expression or a list of them (ANDed).
fn clauses(when: &Node) -> Vec<String> {
    match when {
        Node::Sequence { items, .. } => items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_owned))
            .collect(),
        other => other.as_str().map(str::to_owned).into_iter().collect(),
    }
}

fn is_looped(node: &Node) -> bool {
    node.entries().iter().any(|(k, _)| {
        matches!(k.as_str(), Some(s) if s == "loop" || s.starts_with("with_"))
    })
}

/// `vars_files:` entries. A bare scalar value is Ansible's one-element-list shorthand.
/// Only scalars and one level of nesting are kept: anything deeper, or a non-scalar
/// alternative, fails ansible-core's post-template `isinstance(str)` gate at runtime —
/// it can never name a file, so it is no reference (a future ERROR-rule candidate).
fn vars_files_of(value: &Node) -> Vec<VarsFilesEntry> {
    fn scalar(n: &Node) -> Option<(String, Span)> {
        match n {
            // An empty scalar is a null `vars_files:` key or a `-` with nothing after
            // it — no path to reference.
            Node::Scalar { value, span } if !value.is_empty() => Some((value.clone(), *span)),
            _ => None,
        }
    }
    fn entry(n: &Node) -> Option<VarsFilesEntry> {
        match n {
            Node::Scalar { .. } => {
                let (v, s) = scalar(n)?;
                Some(VarsFilesEntry { alternatives: vec![(v, s)], span: s })
            }
            Node::Sequence { items, span } => {
                let alternatives: Vec<_> = items.iter().filter_map(scalar).collect();
                let end = alternatives.last()?.1.end;
                Some(VarsFilesEntry {
                    alternatives,
                    span: Span { start: span.start, end },
                })
            }
            _ => None,
        }
    }
    match value {
        Node::Sequence { items, .. } => items.iter().filter_map(entry).collect(),
        Node::Scalar { .. } => entry(value).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// The `name: value` bindings under a node's `vars:` mapping.
fn vars_of(node: &Node) -> Vec<VarBinding> {
    node.get("vars")
        .map(|m| {
            m.entries()
                .iter()
                .filter_map(|(k, v)| {
                    Some(VarBinding {
                        name: k.as_str()?.to_string(),
                        span: v.span(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
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
    if let Some(v) = import_playbook_value(node) {
        let when = node.get("when");
        return Some(PlayItem::Import(Import {
            span: v.span(),
            file: v.as_str().map(str::to_owned),
            when: when.map(clauses).unwrap_or_default(),
            when_span: when.map(|w| w.span()),
            vars: import_entry_vars(node),
            directives: collect_directives(node, keywords::is_play_directive),
        }));
    }
    Some(PlayItem::Play(build_play(node)))
}

/// Literal scalars from a `vars:` on an `import_playbook` entry. Non-scalar values are
/// dropped rather than guessed: a nested structure can't be substituted into a path.
fn import_entry_vars(node: &Node) -> Vec<(String, String)> {
    let Some(vars) = node.get("vars") else { return Vec::new() };
    vars.entries()
        .iter()
        .filter_map(|(k, v)| Some((k.as_str()?.to_owned(), v.as_str()?.to_owned())))
        .collect()
}

/// The value of an `import_playbook` key, bare or FQCN. Searched from the end for the same
/// reason as [`Node::get`]: on a duplicate key Ansible imports the last one.
fn import_playbook_value(node: &Node) -> Option<&Node> {
    node.entries()
        .iter()
        .rev()
        .find(|(k, _)| k.as_str().map(keywords::core_action) == Some("import_playbook"))
        .map(|(_, v)| v)
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
        vars: vars_of(node),
        vars_files: node.get("vars_files").map(vars_files_of).unwrap_or_default(),
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
    let when = node.get("when");
    Block {
        span: node.span(),
        name: name_of(node),
        block: stmts("block"),
        rescue: stmts("rescue"),
        always: stmts("always"),
        when: when.map(clauses).unwrap_or_default(),
        when_span: when.map(|w| w.span()),
        vars: vars_of(node),
        directives: collect_directives(node, |k| {
            keywords::is_block_directive(k)
                && k != "name"
                && !keywords::BLOCK_TASK_CONTAINERS.contains(&k)
        }),
    }
}

fn build_task(node: &Node) -> Task {
    let when = node.get("when");
    Task {
        span: node.span(),
        name: name_of(node),
        action: find_action(node),
        when: when.map(clauses).unwrap_or_default(),
        when_span: when.map(|w| w.span()),
        looped: is_looped(node),
        register: node.get("register").and_then(|n| n.as_str()).map(str::to_owned),
        register_span: node.get("register").map(|n| n.span()),
        vars: vars_of(node),
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
                    args: v.clone(),
                });
            }
        }
    }
    for (k, v) in node.entries() {
        let Some(key) = k.as_str() else { continue };
        let is_directive = !key.contains('.') && keywords::is_task_directive(key);
        if !is_directive {
            return Some(Action {
                name: key.to_string(),
                key_span: k.span(),
                args: v.clone(),
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

    fn vars_files(src: &str) -> Vec<VarsFilesEntry> {
        let Ast::Playbook(items) = ast(src) else {
            panic!("expected a playbook");
        };
        let PlayItem::Play(p) = &items[0] else {
            panic!("expected a play");
        };
        p.vars_files.clone()
    }

    #[test]
    fn vars_files_scalar_entries_are_one_alternative_each() {
        let src = "- hosts: all\n  vars_files:\n    - vars/a.yml\n    - b.yml\n";
        let vf = vars_files(src);
        assert_eq!(vf.len(), 2);
        for e in &vf {
            assert_eq!(e.alternatives.len(), 1);
            assert_eq!(e.span, e.alternatives[0].1);
        }
        assert_eq!(vf[0].alternatives[0].0, "vars/a.yml");
        assert_eq!(vf[0].span.slice(src), "vars/a.yml");
        assert_eq!(vf[1].span.slice(src), "b.yml");
    }

    #[test]
    fn vars_files_bare_string_and_null_forms() {
        let vf = vars_files("- hosts: all\n  vars_files: vars/a.yml\n");
        assert_eq!(vf.len(), 1);
        assert_eq!(vf[0].alternatives[0].0, "vars/a.yml");
        assert!(vars_files("- hosts: all\n  vars_files:\n  tasks: []\n").is_empty());
    }

    #[test]
    fn vars_files_nested_list_keeps_alternatives_and_group_span() {
        let src = "- hosts: all\n  vars_files:\n    - - a.yml\n      - b.yml\n    - c.yml\n";
        let vf = vars_files(src);
        assert_eq!(vf.len(), 2);
        assert_eq!(vf[0].alternatives.len(), 2);
        assert_eq!(vf[0].alternatives[1].0, "b.yml");
        // The group span covers the whole nested list from its first `-`, clamped to the
        // last alternative — libyaml's block-sequence end mark would otherwise spill onto
        // the next line.
        assert_eq!(vf[0].span.slice(src), "- a.yml\n      - b.yml");
        assert_eq!(vf[1].alternatives.len(), 1);
    }

    #[test]
    fn vars_files_flow_style_groups() {
        let src = "- hosts: all\n  vars_files: [[a.yml, b.yml], c.yml]\n";
        let vf = vars_files(src);
        assert_eq!(vf.len(), 2);
        assert_eq!(vf[0].alternatives.len(), 2);
        assert_eq!(vf[1].alternatives[0].0, "c.yml");
    }

    #[test]
    fn vars_files_non_scalar_and_deeper_nesting_are_dropped() {
        // A mapping entry, a doubly-nested list, and an empty inner list are all
        // runtime-fatal (or empty) in Ansible — none can name a file, so no entry.
        let src = "- hosts: all\n  vars_files:\n    - {a: b}\n    - - - deep.yml\n    - []\n    - ok.yml\n";
        let vf = vars_files(src);
        assert_eq!(vf.len(), 1);
        assert_eq!(vf[0].alternatives[0].0, "ok.yml");
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

    /// A duplicate key is legal YAML and Ansible keeps the last. Live-verified on
    /// ansible-core 2.21.2: with both `first.yml` and `last.yml` on one entry it warns,
    /// then runs only the tasks from `last.yml`.
    #[test]
    fn duplicate_import_playbook_takes_the_last() {
        let a = ast("- import_playbook: first.yml\n  import_playbook: last.yml\n");
        let Ast::Playbook(items) = a else { panic!("expected a playbook") };
        let PlayItem::Import(imp) = &items[0] else { panic!("expected an import") };
        assert_eq!(imp.file.as_deref(), Some("last.yml"));
    }

    /// Same rule one level down: the `vars:` on the entry is a mapping too, so a repeated
    /// name there resolves to the last value.
    #[test]
    fn duplicate_entry_var_takes_the_last() {
        let a = ast("- import_playbook: \"{{ env }}.yml\"\n  vars:\n    env: dead\n    env: live\n");
        let Ast::Playbook(items) = a else { panic!("expected a playbook") };
        let PlayItem::Import(imp) = &items[0] else { panic!("expected an import") };
        // Both pairs are carried; the resolver folds them into a map, which is last-wins.
        let folded: std::collections::HashMap<_, _> = imp.vars.iter().cloned().collect();
        assert_eq!(folded.get("env").map(String::as_str), Some("live"));
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
