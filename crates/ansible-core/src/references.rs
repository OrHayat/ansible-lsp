//! AST -> cross-file references.

use crate::parse::{Node, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    IncludeTasks,
    ImportTasks,
    /// A role name, from `include_role`/`import_role` or a `roles:` entry.
    Role,
    /// `tasks_from:` — resolves inside whichever role `role` names.
    TasksFrom,
    /// A 3-part FQCN used as a task key, e.g. `volumez.daos.pool_create:`.
    Module,
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
            repeated: false,
            task_name: None,
        }
    }
}

/// What the task around a reference says about whether, and how often, it runs.
#[derive(Default, Clone)]
struct TaskContext {
    conditional: bool,
    repeated: bool,
    name: Option<String>,
}

impl TaskContext {
    fn of(mapping: &Node) -> Self {
        let has = |k: &str| mapping.get(k).is_some();
        Self {
            conditional: has("when"),
            repeated: mapping
                .entries()
                .iter()
                .any(|(k, _)| matches!(k.as_str(), Some(key) if key == "loop" || key.starts_with("with_"))),
            name: mapping.get("name").and_then(|n| n.as_str()).map(str::to_owned),
        }
    }

    fn apply(&self, r: &mut Reference) {
        r.conditional = self.conditional;
        r.repeated = self.repeated;
        r.task_name = self.name.clone();
    }
}

/// `ansible.builtin.include_tasks` -> `include_tasks`
fn short_key(key: &str) -> &str {
    key.rsplit('.').next().unwrap_or(key)
}

pub fn extract(nodes: &[Node]) -> Vec<Reference> {
    let mut out = Vec::new();
    for n in nodes {
        walk(n, &mut out);
    }
    out
}

fn walk(node: &Node, out: &mut Vec<Reference>) {
    match node {
        Node::Sequence { items, .. } => items.iter().for_each(|i| walk(i, out)),
        Node::Mapping { entries, .. } => {
            // `when:`/`loop:` sit on the task, alongside the reference itself.
            let ctx = TaskContext::of(node);
            for (k, v) in entries {
                if let Some(key) = k.as_str() {
                    let before = out.len();
                    handle(key, k, v, out);
                    for r in &mut out[before..] {
                        ctx.apply(r);
                    }
                }
                walk(v, out);
            }
        }
        _ => {}
    }
}

fn handle(key: &str, key_node: &Node, value: &Node, out: &mut Vec<Reference>) {
    match short_key(key) {
        "include_tasks" | "import_tasks" => {
            let kind = if short_key(key) == "include_tasks" {
                ReferenceKind::IncludeTasks
            } else {
                ReferenceKind::ImportTasks
            };
            // `include_tasks: f.yml` and `include_tasks:\n  file: f.yml`
            let target = match value {
                Node::Scalar { .. } => Some(value),
                Node::Mapping { .. } => value.get("file"),
                _ => None,
            };
            if let Some(Node::Scalar { value, span }) = target {
                out.push(Reference::new(kind, value, *span));
            }
        }

        "include_role" | "import_role" => {
            // Block and flow forms are the same AST, so `name` and `tasks_from` are
            // always siblings in one mapping — no line-proximity guessing.
            let name = match value.get("name") {
                Some(Node::Scalar { value, span }) => Some((value.clone(), *span)),
                _ => None,
            };
            let tasks_from = value.get("tasks_from");
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

        "roles" => {
            for item in value.items() {
                match item {
                    // - myrole
                    Node::Scalar { value, span } => {
                        out.push(Reference::new(ReferenceKind::Role, value, *span));
                    }
                    // - role: myrole
                    Node::Mapping { .. } => {
                        if let Some(Node::Scalar { value, span }) = item.get("role") {
                            out.push(Reference::new(ReferenceKind::Role, value, *span));
                        }
                    }
                    _ => {}
                }
            }
        }

        _ => {
            // A 3-part dotted name is only a module when it's a task KEY. Values can
            // look identical — this repo contains the URL `volumez.atlassian.net`.
            if key.split('.').count() == 3 && !key.contains(' ') {
                out.push(Reference::new(
                    ReferenceKind::Module,
                    key,
                    key_node.span(),
                ));
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
    fn roles_block_bare_and_dict_forms() {
        let r = of(
            "- hosts: all\n  roles:\n    - starrocks_setup\n    - role: podman\n",
            ReferenceKind::Role,
        );
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].value, "starrocks_setup");
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
            "- volumez.daos.pool_create:\n    name: p\n- debug:\n    msg: volumez.atlassian.net\n",
            ReferenceKind::Module,
        );
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].value, "volumez.daos.pool_create");
    }
}
