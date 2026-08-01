//! AST -> cross-file references.

use crate::ast::{self, Action, Ast, Import, Play, PlayItem, Stmt, Task};
use crate::parse::{Node, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    IncludeTasks,
    ImportTasks,
    /// A role name, from `include_role`/`import_role` or a `roles:` entry.
    Role,
    /// `tasks_from:` — resolves inside whichever role `role` names.
    TasksFrom,
    /// A 3-part FQCN used as a task key, e.g. `community.lvm.pool_create:`.
    Module,
    /// `import_playbook:` — play-level, static, so never legitimately templated.
    ImportPlaybook,
    /// `include_vars:` file target — a vars file that should exist.
    IncludeVars,
}

#[derive(Debug, Clone)]
pub struct Reference {
    pub kind: ReferenceKind,
    pub value: String,
    /// Jinja expression present, so the target is only knowable at runtime.
    pub templated: bool,
    pub span: Span,
    /// For `TasksFrom`: the role it belongs to, read from the same mapping node.
    pub role: Option<String>,
    /// For `Role`: the same include carried a `tasks_from`, so `tasks/main.yml` is
    /// not required — a role can exist purely as named task files.
    pub has_tasks_from: bool,
    /// The containing task has a `when:`, so this call may not happen.
    pub conditional: bool,
    /// The `when:` clauses themselves, for [`crate::condition`]. A list `when:` is
    /// several clauses ANDed together.
    pub conditions: Vec<String>,
    /// Span of the `when:` value, so a problem with the condition is reported on the
    /// condition rather than on the reference a few lines away.
    pub condition_span: Option<Span>,
    /// The containing task has a `loop:`/`with_*`, so it may happen many times.
    pub repeated: bool,
    /// The containing task's `name:`, for labelling an execution tree.
    pub task_name: Option<String>,
}

impl Reference {
    fn new(kind: ReferenceKind, value: &str, span: Span) -> Self {
        Self {
            kind,
            value: value.to_string(),
            templated: value.contains("{{"),
            span,
            role: None,
            has_tasks_from: false,
            conditional: false,
            conditions: Vec::new(),
            condition_span: None,
            repeated: false,
            task_name: None,
        }
    }
}

/// `ansible.builtin.include_tasks` -> `include_tasks`
fn short_key(key: &str) -> &str {
    key.rsplit('.').next().unwrap_or(key)
}

/// Every cross-file reference in a parsed file. Walks the semantic model
/// ([`crate::ast`]) rather than the raw tree, so a task's module and its `when:`/`loop:`
/// context are read from structure instead of re-detected key by key.
pub fn extract(nodes: &[Node]) -> Vec<Reference> {
    let mut out = Vec::new();
    match ast::build(nodes) {
        Ast::Playbook(items) => items.iter().for_each(|it| play_item(it, &mut out)),
        Ast::Tasks(stmts) => stmts.iter().for_each(|s| stmt(s, &mut out)),
        Ast::Other => {}
    }
    out
}

fn play_item(item: &PlayItem, out: &mut Vec<Reference>) {
    match item {
        PlayItem::Play(p) => play(p, out),
        PlayItem::Import(i) => import_playbook(i, out),
    }
}

fn play(p: &Play, out: &mut Vec<Reference>) {
    for role in &p.roles {
        let mut r = Reference::new(ReferenceKind::Role, &role.name, role.span);
        // A `roles:` entry inherits the play's identity, not a task's.
        r.task_name = p.name.clone();
        out.push(r);
    }
    for s in p
        .pre_tasks
        .iter()
        .chain(&p.tasks)
        .chain(&p.post_tasks)
        .chain(&p.handlers)
    {
        stmt(s, out);
    }
}

fn import_playbook(i: &Import, out: &mut Vec<Reference>) {
    if let Some(file) = &i.file {
        let mut r = Reference::new(ReferenceKind::ImportPlaybook, file, i.span);
        // A `when:` on a static import isn't a gate — it's copied onto every imported task.
        r.conditional = i.when_span.is_some();
        r.conditions = i.when.clone();
        r.condition_span = i.when_span;
        out.push(r);
    }
}

fn stmt(s: &Stmt, out: &mut Vec<Reference>) {
    match s {
        Stmt::Task(t) => task(t, out),
        // A block-level `when:` propagates to each contained task at runtime, but that's
        // the resolver's concern; here a block only nests statements.
        Stmt::Block(b) => b
            .block
            .iter()
            .chain(&b.rescue)
            .chain(&b.always)
            .for_each(|s| stmt(s, out)),
    }
}

fn task(t: &Task, out: &mut Vec<Reference>) {
    let Some(action) = &t.action else { return };
    let before = out.len();
    module_refs(action, out);
    // The task's `when:`/`loop:`/`name:` belong to every reference it produced.
    for r in &mut out[before..] {
        r.conditional = t.when_span.is_some();
        r.conditions = t.when.clone();
        r.condition_span = t.when_span;
        r.repeated = t.looped;
        r.task_name = t.name.clone();
    }
}

/// The reference(s) a task's module implies: an include target, a role + `tasks_from`, or
/// a bare FQCN module.
fn module_refs(a: &Action, out: &mut Vec<Reference>) {
    match short_key(&a.name) {
        "include_tasks" | "import_tasks" => {
            let kind = if short_key(&a.name) == "include_tasks" {
                ReferenceKind::IncludeTasks
            } else {
                ReferenceKind::ImportTasks
            };
            // `include_tasks: f.yml` and `include_tasks:\n  file: f.yml`
            let target = match &a.args {
                Node::Scalar { .. } => Some(&a.args),
                Node::Mapping { .. } => a.args.get("file"),
                _ => None,
            };
            if let Some(Node::Scalar { value, span }) = target {
                out.push(Reference::new(kind, value, *span));
            }
        }

        // `import_playbook` as a task key is unusual, but keep parity with the old walk.
        "import_playbook" => {
            if let Node::Scalar { value, span } = &a.args {
                out.push(Reference::new(ReferenceKind::ImportPlaybook, value, *span));
            }
        }

        "include_role" | "import_role" => {
            let name = match a.args.get("name") {
                Some(Node::Scalar { value, span }) => Some((value.clone(), *span)),
                _ => None,
            };
            let tasks_from = a.args.get("tasks_from");
            if let Some((n, span)) = &name {
                let mut r = Reference::new(ReferenceKind::Role, n, *span);
                r.has_tasks_from = tasks_from.is_some();
                out.push(r);
            }
            if let Some(Node::Scalar { value: from, span }) = tasks_from {
                let mut r = Reference::new(ReferenceKind::TasksFrom, from, *span);
                r.role = name.map(|(n, _)| n);
                out.push(r);
            }
        }

        "include_vars" => {
            // The file form (`x.yml` or `{ file: x.yml }`) is a file that must exist; the
            // dir form (`{ dir: … }`) points at a directory and is left to the var indexer.
            let target = match &a.args {
                Node::Scalar { .. } => Some(&a.args),
                Node::Mapping { .. } if a.args.get("dir").is_none() => a.args.get("file"),
                _ => None,
            };
            if let Some(Node::Scalar { value, span }) = target {
                out.push(Reference::new(ReferenceKind::IncludeVars, value, *span));
            }
        }

        _ => {
            // A 3-part dotted name in module position is a collection FQCN. The old walk
            // matched any 3-part *key*; the AST already knows this key is the module.
            if a.name.split('.').count() == 3 && !a.name.contains(' ') {
                out.push(Reference::new(ReferenceKind::Module, &a.name, a.key_span));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    fn refs(src: &str) -> Vec<Reference> {
        let doc = Document::new(src.to_string());
        extract(&doc.parse().expect("valid yaml"))
    }

    fn of(src: &str, kind: ReferenceKind) -> Vec<Reference> {
        refs(src).into_iter().filter(|r| r.kind == kind).collect()
    }

    #[test]
    fn inline_and_block_forms() {
        let r = of(
            "- include_tasks: a.yml\n- include_tasks:\n    file: b.yml\n",
            ReferenceKind::IncludeTasks,
        );
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].value, "a.yml");
        assert_eq!(r[1].value, "b.yml");
    }

    #[test]
    fn fqcn_normalises_to_the_same_kind() {
        let r = refs("- ansible.builtin.include_tasks: a.yml\n- import_tasks: b.yml\n");
        assert_eq!(r[0].kind, ReferenceKind::IncludeTasks);
        assert_eq!(r[1].kind, ReferenceKind::ImportTasks);
    }

    #[test]
    fn include_vars_file_forms_are_references_dir_form_is_not() {
        let r = of(
            "- include_vars: a.yml\n- include_vars: { file: b.yml }\n- include_vars: { dir: vars }\n",
            ReferenceKind::IncludeVars,
        );
        // Only the two file forms; the dir form is not a file reference.
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].value, "a.yml");
        assert_eq!(r[1].value, "b.yml");
    }

    #[test]
    fn commented_out_includes_are_not_references() {
        let r = refs("# - include_tasks: ghost.yml\n- include_tasks: real.yml\n");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].value, "real.yml");
    }

    #[test]
    fn templated_values_are_flagged_not_dropped() {
        let r = refs("- include_tasks: \"{{ ap_protocol }}/validate.yml\"\n");
        assert!(r[0].templated);
    }

    #[test]
    fn span_points_at_the_value() {
        let src = "- include_tasks: _converge_one_ap.yml\n";
        assert_eq!(refs(src)[0].span.slice(src), "_converge_one_ap.yml");
    }

    #[test]
    fn task_conditions_attach_to_the_reference() {
        let r = refs(
            "- name: maybe\n  include_tasks: a.yml\n  when: x is defined\n\
             - name: many\n  include_tasks: b.yml\n  loop: [1, 2]\n\
             - name: always\n  include_tasks: c.yml\n",
        );
        assert!(r[0].conditional && !r[0].repeated);
        assert!(r[1].repeated && !r[1].conditional);
        assert!(!r[2].conditional && !r[2].repeated);
        assert_eq!(r[0].task_name.as_deref(), Some("maybe"));
    }

    #[test]
    fn with_items_counts_as_repeated() {
        let r = refs("- include_tasks: a.yml\n  with_items: [1]\n");
        assert!(r[0].repeated);
    }

    #[test]
    fn import_playbook_is_extracted() {
        let r = of(
            "- name: infra\n  import_playbook: lustre-infrastructure.yml\n  when: x\n\
             - ansible.builtin.import_playbook: ../../network-setup.yml\n",
            ReferenceKind::ImportPlaybook,
        );
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].value, "lustre-infrastructure.yml");
        assert_eq!(r[1].value, "../../network-setup.yml");
        // `when:` on a static import is pushed onto every imported task, not a gate.
        assert!(r[0].conditional);
    }

    #[test]
    fn roles_block_bare_and_dict_forms() {
        let r = of(
            "- hosts: all\n  roles:\n    - postgres_setup\n    - role: podman\n",
            ReferenceKind::Role,
        );
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].value, "postgres_setup");
        assert_eq!(r[1].value, "podman");
    }

    /// The prototype scans ±6 lines for a sibling `name:`, so the flow form — where
    /// everything is on one line — silently fails.
    #[test]
    fn tasks_from_binds_to_its_own_role_in_flow_form() {
        let r = of(
            "- include_role: { name: cib-batch, tasks_from: begin }\n",
            ReferenceKind::TasksFrom,
        );
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].value, "begin");
        assert_eq!(r[0].role.as_deref(), Some("cib-batch"));
    }

    #[test]
    fn tasks_from_binds_correctly_across_adjacent_tasks() {
        // Two roles in a row: the second tasks_from must not bind to the first name.
        let r = of(
            "- include_role:\n    name: alpha\n    tasks_from: one\n\
             - include_role:\n    name: beta\n    tasks_from: two\n",
            ReferenceKind::TasksFrom,
        );
        assert_eq!(r[0].role.as_deref(), Some("alpha"));
        assert_eq!(r[1].role.as_deref(), Some("beta"));
    }

    #[test]
    fn fqcn_module_keys_are_references_but_urls_are_not() {
        let r = of(
            "- community.lvm.pool_create:\n    name: p\n- debug:\n    msg: example.atlassian.net\n",
            ReferenceKind::Module,
        );
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].value, "community.lvm.pool_create");
    }
}
