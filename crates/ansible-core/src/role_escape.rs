//! T-185: a role file whose task or vars include resolves outside its own role.
//!
//! It runs — the file is there — so nothing is wrong today. What is wrong is that the role now
//! depends on the layout around it: moving it, vendoring it, or packaging it into a collection
//! breaks the include, at run time and in someone else's tree. A judgement about portability,
//! not a fault, so a HINT.
//!
//! Decided on the **resolved** target, not the text: `tasks/../tasks/x.yml` stays inside.
//!
//! Left alone, each legitimate: a playbook including anything; `include_role` / `import_role`,
//! which is how roles compose; a file inside another role; anything unresolved.
//!
//! Also left alone: any templated path. From a role, `{{ playbook_dir }}` is whichever playbook
//! invoked it, and the resolver can only guess that set (the project root and `playbooks/`,
//! until T-137) — a hint naming where the include lands would be naming a guess. `{{ role_path
//! }}` is not expanded at all (T-068). The corpus's nine `{{ playbook_dir }}/../<manifest>`
//! reads from roles are non-portable, and are not reported for that reason.
//!
//! "In a role" means the role directory sits under a `roles/` directory. `FileContext` reads
//! any directory with a `tasks/` inside as a role — the corpus has a `playbooks/tasks/`, which
//! makes every playbook next to it a "role file" — and that is too loose to judge portability
//! on. A collection's `roles/<name>` qualifies the same way.

use std::path::{Component, Path, PathBuf};

use crate::parse::Span;
use crate::references::{Reference, ReferenceKind};
use crate::resolve::{Resolution, Status};

/// Rule id, for `# noqa: role-include-escapes-role` and for display.
pub const RULE_ID: &str = "role-include-escapes-role";

#[derive(Debug, Clone)]
pub struct Problem {
    /// The include's path value.
    pub span: Span,
    pub message: String,
    pub rule: &'static str,
}

pub fn problems(
    refs: &[(Reference, Resolution)],
    role_dir: Option<&Path>,
    is_playbook: bool,
) -> Vec<Problem> {
    let Some(role) = role_dir.filter(|r| under_roles_dir(r)) else { return Vec::new() };
    if is_playbook {
        return Vec::new();
    }
    refs.iter()
        .filter(|(r, _)| {
            matches!(
                r.kind,
                ReferenceKind::IncludeTasks
                    | ReferenceKind::ImportTasks
                    | ReferenceKind::IncludeVars
                    | ReferenceKind::IncludeVarsDir
            )
        })
        .filter(|(r, res)| !r.templated && res.status == Status::Resolved && !res.targets.is_empty())
        .filter(|(_, res)| {
            res.targets.iter().map(|t| normalise(t)).all(|t| !t.starts_with(role) && !in_any_role(&t))
        })
        .map(|(r, res)| {
            let lands = res.targets[0].to_string_lossy().replace('\\', "/");
            let role_name = role.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
            Problem {
                span: r.span,
                message: format!(
                    "this include resolves outside the role `{role_name}`, to `{lands}` — the role \
                     only works in this repo's layout. Moving it, vendoring it or packaging it \
                     into a collection breaks the include at run time."
                ),
                rule: RULE_ID,
            }
        })
        .collect()
}

/// Lexical `..` removal. A literal include's target arrives normalised, a glob match does not
/// (`role/tasks/../../../tasks/x.yml` measured) — and that one starts with the role textually.
fn normalise(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The directory is a role by the one test that does not misfire: it sits in a `roles/` dir.
fn under_roles_dir(role: &Path) -> bool {
    role.parent().and_then(Path::file_name).is_some_and(|n| n == "roles")
}

/// Inside some role — the target's own, or another. Reaching into another role is untidy but a
/// real pattern, and out of scope.
fn in_any_role(target: &Path) -> bool {
    target.ancestors().skip(1).any(under_roles_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;
    use crate::workspace::FileContext;

    /// The value of each flagged include, for `text` read as if it were the file at `rel`
    /// (demo-relative), resolved the way the editor resolves it.
    fn flagged_at(rel: &str, text: &str) -> Vec<String> {
        let path = Path::new("../../demo").join(rel).canonicalize().unwrap();
        let ctx = FileContext::discover(&path);
        let nodes = Document::new(text.into()).parse().unwrap();
        let ex = crate::references::extract(&nodes);
        let resolver = crate::resolve::Resolver { in_playbook: ex.in_playbook, ..Default::default() };
        let refs: Vec<_> = ex.refs.into_iter().map(|r| {
            let res = resolver.resolve(&r, &ctx);
            (r, res)
        }).collect();
        problems(&refs, ctx.role_dir.as_deref(), ex.in_playbook)
            .into_iter()
            .map(|p| p.span.slice(text).to_string())
            .collect()
    }

    fn in_role(text: &str) -> Vec<String> {
        flagged_at("roles/escapee/tasks/main.yml", text)
    }

    #[test]
    fn an_include_tasks_outside_the_role_fires() {
        assert_eq!(in_role("- include_tasks: ../../../tasks/sibling.yml\n"), vec!["../../../tasks/sibling.yml"]);
    }

    #[test]
    fn import_tasks_and_include_vars_fire_too() {
        assert_eq!(in_role("- import_tasks: ../../../tasks/sibling.yml\n").len(), 1);
        assert_eq!(in_role("- include_vars: ../../../vars/shared.yml\n").len(), 1);
    }

    /// Resolved, not textual: `..` that lands back inside the role is fine.
    #[test]
    fn a_path_that_normalises_back_inside_is_silent() {
        assert_eq!(in_role("- include_tasks: inner.yml\n"), Vec::<String>::new());
        assert_eq!(in_role("- include_tasks: ../tasks/inner.yml\n"), Vec::<String>::new());
    }

    /// From a role, `playbook_dir`'s value is a guess (T-137), and `role_path` is not expanded
    /// (T-068). A hint naming where the include lands would name a guess.
    #[test]
    fn a_templated_path_is_silent() {
        // Control: this one does resolve, outside the role — only the templated test stops it.
        let path = Path::new("../../demo/roles/escapee/tasks/main.yml").canonicalize().unwrap();
        let ctx = FileContext::discover(&path);
        let text = "- include_tasks: \"{{ playbook_dir }}/tasks/sibling.yml\"\n";
        let r = crate::references::extract(&Document::new(text.into()).parse().unwrap()).refs.remove(0);
        let res = crate::resolve::Resolver::default().resolve(&r, &ctx);
        assert!(res.targets.iter().any(|t| t.ends_with("demo/tasks/sibling.yml")), "control: {res:?}");
        assert_eq!(in_role(text), Vec::<String>::new());
        assert_eq!(in_role("- include_tasks: \"{{ role_path }}/../../tasks/sibling.yml\"\n"), Vec::<String>::new());
    }

    /// A glob's targets keep their `..`, so they textually start inside the role. Normalised
    /// before the check, or an escape through a pattern would read as staying home.
    #[test]
    fn targets_are_normalised_before_the_check() {
        let role = Path::new("/r/roles/x");
        assert!(!normalise(Path::new("/r/roles/x/tasks/../../../tasks/a.yml")).starts_with(role));
        assert!(normalise(Path::new("/r/roles/x/tasks/../tasks/a.yml")).starts_with(role));
    }

    /// Out of scope, one assertion each (the ticket's list).
    #[test]
    fn another_roles_file_is_silent() {
        assert_eq!(in_role("- include_tasks: ../../reporting/tasks/main.yml\n"), Vec::<String>::new());
    }

    #[test]
    fn include_role_is_silent() {
        assert_eq!(in_role("- include_role:\n    name: reporting\n"), Vec::<String>::new());
    }

    /// A play is judged by no role, even one written inside a role's folder — where the same
    /// include does resolve outside the role (asserted in the control below).
    #[test]
    fn a_playbook_including_anything_is_silent() {
        let text = "- hosts: all\n  tasks:\n    - include_tasks: ../../../tasks/sibling.yml\n";
        assert_eq!(in_role(text), Vec::<String>::new());
        assert_eq!(in_role("- include_tasks: ../../../tasks/sibling.yml\n").len(), 1, "control");
    }

    /// A folder with a `tasks/` inside is not a role unless it sits under `roles/`. The corpus
    /// has a `playbooks/tasks/`, and `FileContext` hands over `playbooks/` as the role dir for
    /// every playbook beside it. Modelled here by passing a loose "role" the same way: the
    /// include does land outside it, so only the `roles/` test keeps this silent.
    #[test]
    fn a_role_dir_not_under_roles_is_not_judged() {
        let demo = Path::new("../../demo").canonicalize().unwrap();
        let ctx = FileContext::discover(&demo.join("tasks/main.yml"));
        let text = "- include_vars: ../vars/shared.yml\n";
        let nodes = Document::new(text.into()).parse().unwrap();
        let refs: Vec<_> = crate::references::extract(&nodes).refs.into_iter().map(|r| {
            let res = crate::resolve::Resolver::default().resolve(&r, &ctx);
            (r, res)
        }).collect();
        assert!(
            refs[0].1.targets.first().is_some_and(|t| t.starts_with(demo.join("vars"))),
            "control: the include resolves, outside demo/tasks: {:?}",
            refs[0].1
        );
        assert!(problems(&refs, Some(&demo.join("tasks")), false).is_empty());
    }

    #[test]
    fn an_unresolved_include_is_silent() {
        assert_eq!(in_role("- include_tasks: ../../../tasks/no_such_file.yml\n"), Vec::<String>::new());
    }

    #[test]
    fn a_collection_qualified_role_is_silent() {
        assert_eq!(in_role("- include_role:\n    name: demo.charlie.nothing\n"), Vec::<String>::new());
    }

    #[test]
    fn the_message_names_where_it_lands_and_the_consequence() {
        let path = Path::new("../../demo/roles/escapee/tasks/main.yml").canonicalize().unwrap();
        let ctx = FileContext::discover(&path);
        let text = "- include_tasks: ../../../tasks/sibling.yml\n";
        let nodes = Document::new(text.into()).parse().unwrap();
        let ex = crate::references::extract(&nodes);
        let refs: Vec<_> = ex.refs.into_iter().map(|r| {
            let res = crate::resolve::Resolver::default().resolve(&r, &ctx);
            (r, res)
        }).collect();
        let p = &problems(&refs, ctx.role_dir.as_deref(), false)[0];
        assert_eq!(p.rule, RULE_ID);
        assert!(p.message.contains("sibling.yml"), "{}", p.message);
        assert!(p.message.contains("outside the role"), "{}", p.message);
    }
}
