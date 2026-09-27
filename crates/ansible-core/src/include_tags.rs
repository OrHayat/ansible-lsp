//! T-230: `tags:` on a dynamic include tags the include line, not the tasks it brings in.
//!
//! Measured on ansible-core 2.21.3 with `--tags web`: `tags: [web]` on an `include_tasks` or
//! `include_role` runs the include and skips every task inside. Only `apply: {tags: [...]}`
//! in the include's arguments carries a tag through, and it has to name that tag — `apply`
//! tagged `other`, or with no `tags:` at all, still skips them. `always` behaves the same:
//! the include runs under any `--tags`, its tasks do not.
//!
//! What does reach the included tasks, and so stays silent: a tag on an enclosing `block:` or
//! on the play, any static import or `roles:` entry (their tags are copied onto each task),
//! and an include in `handlers:` — a notified handler's include ran its tasks under `--tags`.
//! `never` alone is the author switching the include off, not a tag they meant to carry.
//!
//! The rule reads the included file too. A task written with the tag itself runs under
//! `--tags` whatever the include says (measured), and the corpus tags both sides on purpose:
//! 21 of 116 local-only hits were includes whose every task already carried the tag. So a
//! warning needs at least one task inside that lacks it, and names how many. When the target
//! cannot be read the rule is silent — the tag still stops at the line, but whether anything
//! is skipped is unknown.

use crate::ast::{Ast, Directive, PlayItem, Stmt, Task};
use crate::keywords;
use crate::parse::{node_with_span, Node, Span};

/// Rule id, for `# noqa: include-tags-not-applied` and for display.
pub const RULE_ID: &str = "include-tags-not-applied";

#[derive(Debug, Clone)]
pub struct Problem {
    /// The `tags:` key.
    pub span: Span,
    pub message: String,
    pub rule: &'static str,
}

/// The tags each task in a file runs with: its own plus its enclosing blocks'. `None` for a
/// task whose tags are templated, since what it carries is unknown. See [`leaf_tags`].
pub type LeafTags = Vec<Option<Vec<String>>>;

/// Every include whose `tags:` leave some of its tasks behind. `handlers_file`: the file is a
/// handlers list (a role's `handlers/`), where a notified include runs its tasks anyway.
/// `inner` answers, for one include task, the [`leaf_tags`] of the file it brings in — or
/// `None` when that file cannot be read, which silences the include.
pub fn problems(
    ast: &Ast,
    nodes: &[Node],
    handlers_file: bool,
    inner: &dyn Fn(&Task) -> Option<LeafTags>,
) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut walk = |s: &Stmt| stmt(s, nodes, inner, &mut out);
    match ast {
        Ast::Playbook(items) => {
            for item in items {
                let PlayItem::Play(p) = item else { continue };
                p.pre_tasks.iter().chain(&p.tasks).chain(&p.post_tasks).for_each(&mut walk);
            }
        }
        Ast::Tasks(stmts) if !handlers_file => stmts.iter().for_each(&mut walk),
        _ => {}
    }
    out
}

/// The tasks a task file runs, each with the tags it carries — the input [`problems`]'s
/// `inner` hands back for an include's target. A block's tags reach its tasks (measured), so
/// they are folded in. An include or import inside counts as one task: it runs or not by its
/// own tags. `None` when the file is not a task list.
pub fn leaf_tags(nodes: &[Node]) -> Option<LeafTags> {
    let Ast::Tasks(stmts) = crate::ast::build(nodes) else { return None };
    let mut out = Vec::new();
    for s in &stmts {
        leaves(s, Some(Vec::new()), nodes, &mut out);
    }
    Some(out)
}

fn leaves(s: &Stmt, inherited: Option<Vec<String>>, nodes: &[Node], out: &mut LeafTags) {
    let with_own = |directives: &[Directive]| -> Option<Vec<String>> {
        let mut tags = inherited.clone()?;
        if let Some(d) = directives.iter().rev().find(|d| d.key == "tags") {
            tags.extend(node_with_span(nodes, d.value).and_then(tag_set)?);
        }
        Some(tags)
    };
    match s {
        Stmt::Block(b) => {
            let tags = with_own(&b.directives);
            for child in b.block.iter().chain(&b.rescue).chain(&b.always) {
                leaves(child, tags.clone(), nodes, out);
            }
        }
        Stmt::Task(t) => out.push(with_own(&t.directives)),
    }
}

fn stmt(s: &Stmt, nodes: &[Node], inner: &dyn Fn(&Task) -> Option<LeafTags>, out: &mut Vec<Problem>) {
    match s {
        Stmt::Block(b) => {
            for child in b.block.iter().chain(&b.rescue).chain(&b.always) {
                stmt(child, nodes, inner, out);
            }
        }
        Stmt::Task(t) => task(t, nodes, inner, out),
    }
}

fn task(t: &Task, nodes: &[Node], inner: &dyn Fn(&Task) -> Option<LeafTags>, out: &mut Vec<Problem>) {
    let Some(action) = &t.action else { return };
    if !matches!(keywords::core_action(&action.name), "include_tasks" | "include_role") {
        return;
    }
    // The last `tags:` is the one Ansible loads.
    let Some(d) = t.directives.iter().rev().find(|d| d.key == "tags") else { return };
    let Some(outer) = node_with_span(nodes, d.value).and_then(tag_set) else { return };
    let applied = match action.args.get("apply").and_then(|a| a.get("tags")) {
        Some(n) => match tag_set(n) {
            Some(set) => set,
            None => return,
        },
        None => Vec::new(),
    };
    let missing: Vec<&str> = outer
        .iter()
        .map(String::as_str)
        .filter(|t| *t != "never" && !applied.iter().any(|a| a == t))
        .collect();
    if missing.is_empty() {
        return;
    }
    let Some(tasks) = inner(t) else { return };
    // A task runs under `--tags x` when it carries `x` or `always`. An unknown (templated)
    // set is given the benefit of the doubt.
    let skipped = |tag: &str| {
        tasks
            .iter()
            .filter(|leaf| leaf.as_ref().is_some_and(|l| !l.iter().any(|x| x == tag || x == "always")))
            .count()
    };
    let lost: Vec<(&str, usize)> =
        missing.iter().map(|t| (*t, skipped(t))).filter(|(_, n)| *n > 0).collect();
    let Some(&(first, n)) = lost.first() else { return };
    let selects = if first == "always" {
        "under any `--tags`".to_string()
    } else {
        format!("under `--tags {first}`")
    };
    let names = lost.iter().map(|(t, _)| format!("`{t}`")).collect::<Vec<_>>().join(", ");
    let total = tasks.len();
    let lost_count = match (n, total) {
        (1, 1) => "its only task is".to_string(),
        (n, t) if n == t => format!("all {t} of its tasks are"),
        (n, t) => format!("{n} of its {t} tasks are"),
    };
    out.push(Problem {
        span: d.key_span,
        message: format!(
            "{selects} this include runs but {lost_count} skipped: tags on a dynamic include \
             stop at the include line, and those tasks are not tagged themselves. Not carried \
             through: {names}. `apply: {{tags: [...]}}` in the include's arguments reaches \
             every task."
        ),
        rule: RULE_ID,
    });
}

/// The tags a `tags:` value names: a list, or a comma-separated string (measured: `"web,db"`
/// is two tags, spaces trimmed). `None` when any part is templated — the set is unknown.
fn tag_set(n: &Node) -> Option<Vec<String>> {
    let scalars: Vec<&str> = match n {
        Node::Scalar { value, .. } => vec![value.as_str()],
        Node::Sequence { items, .. } => items.iter().map(|i| i.as_str()).collect::<Option<_>>()?,
        _ => return None,
    };
    if scalars.iter().any(|s| s.contains("{{") || s.contains("{%")) {
        return None;
    }
    Some(
        scalars
            .iter()
            .flat_map(|s| s.split(','))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    /// `(line, message)` per warning, with every include bringing in `target`'s tasks.
    fn run_into(src: &str, handlers_file: bool, target: Option<&str>) -> Vec<(usize, String)> {
        let doc = Document::new(src.into());
        let nodes = doc.parse().unwrap();
        let leaves = target.map(|t| leaf_tags(&Document::new(t.into()).parse().unwrap()).unwrap());
        problems(&crate::ast::build(&nodes), &nodes, handlers_file, &|_| leaves.clone())
            .into_iter()
            .map(|p| (doc.byte_to_lsp(p.span.start).0 as usize, p.message))
            .collect()
    }

    /// The included file most tests use: one task with no tags, so anything the include line
    /// fails to carry is lost.
    const UNTAGGED: &str = "- debug: msg=x\n";

    fn run_in(src: &str, handlers_file: bool) -> Vec<(usize, String)> {
        run_into(src, handlers_file, Some(UNTAGGED))
    }

    fn lines(src: &str) -> Vec<usize> {
        run_in(src, false).into_iter().map(|(l, _)| l).collect()
    }

    fn play(tasks: &str) -> String {
        format!("- hosts: all\n  tasks:\n{tasks}")
    }

    #[test]
    fn tags_on_include_tasks_warns_on_the_tags_key() {
        let src = play("    - include_tasks: x.yml\n      tags: [web]\n");
        assert_eq!(lines(&src), vec![3]);
        let src = "- hosts: all\n  tasks:\n    - include_tasks: x.yml\n      tags: [web]\n";
        let doc = Document::new(src.into());
        let nodes = doc.parse().unwrap();
        let leaves = leaf_tags(&Document::new(UNTAGGED.into()).parse().unwrap());
        let p = &problems(&crate::ast::build(&nodes), &nodes, false, &|_| leaves.clone())[0];
        assert_eq!(p.span.slice(src), "tags", "anchored on the key");
        assert_eq!(p.rule, RULE_ID);
    }

    #[test]
    fn tags_on_include_role_warns() {
        assert_eq!(lines(&play("    - include_role:\n        name: r\n      tags: [web]\n")), vec![4]);
    }

    #[test]
    fn every_spelling_of_the_action_warns() {
        for action in ["ansible.builtin.include_tasks", "ansible.legacy.include_tasks"] {
            let src = play(&format!("    - {action}: x.yml\n      tags: [web]\n"));
            assert_eq!(lines(&src), vec![3], "{action}");
        }
    }

    #[test]
    fn a_scalar_tag_warns() {
        assert_eq!(lines(&play("    - include_tasks: x.yml\n      tags: web\n")), vec![3]);
    }

    /// The control: the same line with `apply: tags:` covering it is silent.
    #[test]
    fn apply_tags_covering_the_outer_tags_is_silent() {
        let src = play(
            "    - include_tasks:\n        file: x.yml\n        apply:\n          tags: [web]\n      tags: [web]\n",
        );
        assert_eq!(lines(&src), Vec::<usize>::new());
        let src = play(
            "    - include_role:\n        name: r\n        apply: {tags: [web, db]}\n      tags: [db, web]\n",
        );
        assert_eq!(lines(&src), Vec::<usize>::new(), "order does not matter");
    }

    /// Measured: `apply: {tags: [other]}` with `tags: [web]` still skips the inner tasks
    /// under `--tags web`. The message names the tag that does not reach.
    #[test]
    fn apply_tags_missing_an_outer_tag_warns_naming_it() {
        let src = play(
            "    - include_tasks:\n        file: x.yml\n        apply: {tags: [other]}\n      tags: [web, other]\n",
        );
        let got = run_in(&src, false);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].1.contains("`web`"), "names the tag that does not reach: {}", got[0].1);
        assert!(!got[0].1.contains("`other`"), "not the one that does: {}", got[0].1);
    }

    #[test]
    fn apply_without_tags_warns() {
        let src = play("    - include_tasks:\n        file: x.yml\n        apply: {when: true}\n      tags: [web]\n");
        assert_eq!(lines(&src), vec![5]);
    }

    /// Measured: `always` runs the include under any `--tags`, and still skips its tasks.
    #[test]
    fn always_warns() {
        assert_eq!(lines(&play("    - include_tasks: x.yml\n      tags: [always]\n")), vec![3]);
    }

    /// `never` alone switches the include off; there is no tag the author meant to carry.
    /// Beside a real tag, the real tag still does not reach.
    #[test]
    fn never_alone_is_silent_but_not_beside_another_tag() {
        assert_eq!(lines(&play("    - include_tasks: x.yml\n      tags: [never]\n")), Vec::<usize>::new());
        assert_eq!(lines(&play("    - include_tasks: x.yml\n      tags: [never, web]\n")), vec![3]);
    }

    #[test]
    fn templated_tags_are_silent() {
        assert_eq!(lines(&play("    - include_tasks: x.yml\n      tags: \"{{ t }}\"\n")), Vec::<usize>::new());
        assert_eq!(lines(&play("    - include_tasks: x.yml\n      tags: [\"{{ t }}\"]\n")), Vec::<usize>::new());
    }

    /// Measured: imports copy their tags onto every task they bring in.
    #[test]
    fn imports_are_silent() {
        assert_eq!(lines(&play("    - import_tasks: x.yml\n      tags: [web]\n")), Vec::<usize>::new());
        assert_eq!(lines(&play("    - import_role:\n        name: r\n      tags: [web]\n")), Vec::<usize>::new());
    }

    #[test]
    fn a_roles_entry_is_silent() {
        let src = "- hosts: all\n  roles:\n    - role: r\n      tags: [web]\n";
        assert_eq!(lines(src), Vec::<usize>::new());
    }

    /// Measured: a block's or a play's tags DO reach the included tasks. Only a tag written
    /// on the include line itself stops at the line.
    #[test]
    fn inherited_tags_are_silent() {
        let block = play("    - block:\n        - include_tasks: x.yml\n      tags: [web]\n");
        assert_eq!(lines(&block), Vec::<usize>::new());
        let src = "- hosts: all\n  tags: [web]\n  tasks:\n    - include_tasks: x.yml\n";
        assert_eq!(lines(src), Vec::<usize>::new());
    }

    /// Nested inside a block, a tag on the include line itself still warns.
    #[test]
    fn an_include_inside_a_block_is_checked() {
        let src = play("    - block:\n        - include_tasks: x.yml\n          tags: [web]\n");
        assert_eq!(lines(&src), vec![4]);
    }

    /// Measured: a notified handler's include runs its tasks under `--tags web`.
    #[test]
    fn handlers_are_silent() {
        let src = "- hosts: all\n  handlers:\n    - name: h\n      include_tasks: x.yml\n      tags: [web]\n";
        assert_eq!(lines(src), Vec::<usize>::new());
        let file = "- name: h\n  include_tasks: x.yml\n  tags: [web]\n";
        assert_eq!(run_in(file, true), vec![], "a role's handlers/ file");
    }

    /// A role's `tasks/` file is walked like a playbook's task list.
    #[test]
    fn a_task_file_is_checked() {
        let file = "- include_tasks: x.yml\n  tags: [web]\n";
        assert_eq!(run_in(file, false).len(), 1);
    }

    #[test]
    fn the_message_says_what_happens_and_how_to_carry_the_tag() {
        let got = run_in(&play("    - include_tasks: x.yml\n      tags: [web]\n"), false);
        let m = &got[0].1;
        assert!(m.contains("--tags web"), "{m}");
        assert!(m.contains("its only task is skipped"), "{m}");
        assert!(m.contains("apply"), "{m}");
    }

    // ---- what the included file already carries (the corpus gate's 21 false positives) ----

    fn web_include() -> String {
        play("    - include_tasks: x.yml\n      tags: [web]\n")
    }

    /// Measured: tasks tagged `web` themselves run under `--tags web` through an untagged-through
    /// include. Tagging both sides is a deliberate pattern, and correct.
    #[test]
    fn every_inner_task_tagged_itself_is_silent() {
        let inner = "- debug: msg=a\n  tags: [web]\n- debug: msg=b\n  tags: [db, web]\n";
        assert_eq!(run_into(&web_include(), false, Some(inner)), vec![]);
    }

    #[test]
    fn inner_tasks_tagged_always_are_silent() {
        let inner = "- debug: msg=a\n  tags: always\n";
        assert_eq!(run_into(&web_include(), false, Some(inner)), vec![]);
    }

    /// A block's tags reach its tasks, so a tagged block inside covers them.
    #[test]
    fn an_inner_block_tag_covers_its_tasks() {
        let inner = "- block:\n    - debug: msg=a\n    - debug: msg=b\n  tags: [web]\n";
        assert_eq!(run_into(&web_include(), false, Some(inner)), vec![]);
    }

    /// Some tasks tagged, some not: the untagged ones are skipped, and the message counts them.
    #[test]
    fn partly_tagged_inner_tasks_warn_with_the_count() {
        let inner = "- debug: msg=a\n  tags: [web]\n- debug: msg=b\n- block:\n    - debug: msg=c\n";
        let got = run_into(&web_include(), false, Some(inner));
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].1.contains("2 of its 3 tasks are skipped"), "{}", got[0].1);
    }

    /// Only the tags that actually lose tasks are named.
    #[test]
    fn a_tag_every_inner_task_carries_is_not_named() {
        let src = play("    - include_tasks: x.yml\n      tags: [web, db]\n");
        let inner = "- debug: msg=a\n  tags: [db]\n";
        let got = run_into(&src, false, Some(inner));
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].1.contains("`web`") && !got[0].1.contains("`db`"), "{}", got[0].1);
    }

    /// An unreadable target — missing, templated, a collection role — leaves the outcome
    /// unknown, so the rule says nothing.
    #[test]
    fn an_unreadable_target_is_silent() {
        assert_eq!(run_into(&web_include(), false, None), vec![]);
    }

    /// An inner task whose tags are templated might carry the tag; not counted as skipped.
    #[test]
    fn a_templated_inner_tag_is_not_counted_as_skipped() {
        let inner = "- debug: msg=a\n  tags: \"{{ t }}\"\n";
        assert_eq!(run_into(&web_include(), false, Some(inner)), vec![]);
    }

    /// An empty target skips nothing. (`include_target` already reports the empty file.)
    #[test]
    fn an_empty_task_list_is_silent() {
        assert_eq!(run_into(&web_include(), false, Some("[]\n")), vec![]);
    }

    #[test]
    fn every_inner_task_untagged_says_all() {
        let got = run_into(&web_include(), false, Some("- debug: msg=a\n- debug: msg=b\n"));
        assert!(got[0].1.contains("all 2 of its tasks are skipped"), "{}", got[0].1);
    }
}
