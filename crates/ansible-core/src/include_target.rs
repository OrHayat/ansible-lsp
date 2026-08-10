//! T-110 rows 5 and 23: what the file an import or include points *at* has to contain.
//!
//! Every other placement rule decides from one document. These two cannot — the fault is in a
//! second file, and the reference is what proves the second file is a **task file** at all.
//! That is the part worth noticing: `placement.rs` gives up on a standalone file because
//! content alone cannot say what kind of file it is, and an `import_tasks:` pointing at it
//! answers exactly that question.
//!
//! Upstream is two branches of one read (`helpers.py:209-214`):
//!
//! ```python
//! data = loader.load_from_file(include_file, trusted_as_template=True)
//! if not data:
//!     display.warning('file %s is empty and had no tasks to include' % include_file)
//!     continue
//! elif not isinstance(data, list):
//!     raise AnsibleParserError("included task files must contain a list of tasks", obj=data)
//! ```
//!
//! `not data` is Python truthiness, not "zero bytes", and that decides the split: `[]`, `{}`
//! and a comments-only file are all **empty** rather than not-a-list. Measured, all four give
//! the warning; a non-empty mapping or a bare scalar gives the error.
//!
//! Both rules cover `include_tasks` as well, which upstream does not — measured on 2.21.2:
//!
//! | target      | `import_tasks`      | `include_tasks`                  |
//! | ----------- | ------------------- | -------------------------------- |
//! | not a list  | error at load       | the same message, at run time    |
//! | empty       | warning at load     | silent in both phases            |
//!
//! So the include spelling is the more useful of the two to catch here: the same mistake, with
//! the feedback arriving mid-run or never.

use crate::parse::{Node, Span};
use crate::placement::{Problem, Tier};
use crate::references::ReferenceKind;

/// Row 5. Verbatim upstream, and identical for both spellings — only the timing differs.
pub const NOT_A_LIST_RULE_ID: &str = "invalid-task-file";
/// Row 23. Ours for `include_tasks`, which has no upstream message, so one id covers both.
pub const EMPTY_RULE_ID: &str = "empty-task-file";

const NOT_A_LIST: &str = "included task files must contain a list of tasks";

/// The fault in `target`, anchored on `span` — the reference in the file being edited, not a
/// position in the file at fault, which may not even be open.
///
/// `target` is every document libyaml found. Ansible reads one (`get_single_data`), so only
/// the first is judged.
pub fn problem(kind: ReferenceKind, target: &[Node], span: Span) -> Option<Problem> {
    let verb = match kind {
        ReferenceKind::ImportTasks => "imports",
        ReferenceKind::IncludeTasks => "includes",
        _ => return None,
    };
    let root = target.first();
    if falsy(root) {
        // Ansible's own warning names the resolved path. Ours anchors on the reference, where
        // the path is already written, so it says what the author gains from knowing instead:
        // that this line does nothing, and whether they would ever have been told.
        let tail = match kind {
            ReferenceKind::ImportTasks => "Ansible warns and carries on.",
            _ => "Ansible says nothing at all.",
        };
        return Some(Problem {
            span,
            tier: Tier::Warning,
            message: format!("the file this {verb} is empty — no tasks come from it. {tail}"),
            rule: EMPTY_RULE_ID,
        });
    }
    match root {
        Some(Node::Sequence { .. }) => None,
        // An alias at the document root resolves to whatever the anchor holds. T-160.
        Some(Node::Other { .. }) | None => None,
        _ => Some(Problem {
            span,
            tier: Tier::Error,
            message: NOT_A_LIST.into(),
            rule: NOT_A_LIST_RULE_ID,
        }),
    }
}

/// Python's `not data` on the loaded document, which is what upstream branches on — an empty
/// container is falsy there, so `[]` and `{}` are "empty", not "not a list".
fn falsy(root: Option<&Node>) -> bool {
    match root {
        None | Some(Node::Null { .. }) => true,
        Some(Node::Sequence { items, .. }) => items.is_empty(),
        Some(Node::Mapping { entries, .. }) => entries.is_empty(),
        Some(Node::Scalar { .. }) => root.and_then(Node::as_str).is_some_and(str::is_empty),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    fn judge(kind: ReferenceKind, src: &str) -> Option<Problem> {
        let nodes = Document::new(src.to_string()).parse().expect("target parses");
        problem(kind, &nodes, Span { start: 0, end: 1 })
    }

    /// Each shape upstream treats as falsy, so each is row 23 and not row 5.
    #[test]
    fn every_falsy_document_is_empty_rather_than_not_a_list() {
        for src in ["", "---\n", "# just a comment\n", "[]\n", "{}\n", "''\n"] {
            let got = judge(ReferenceKind::ImportTasks, src).expect("a problem");
            assert_eq!(got.rule, EMPTY_RULE_ID, "{src:?}");
            assert_eq!(got.tier, Tier::Warning, "{src:?}");
        }
    }

    #[test]
    fn a_truthy_non_list_is_row_5() {
        for src in ["a: b\n", "hello\n", "42\n"] {
            let got = judge(ReferenceKind::ImportTasks, src).expect("a problem");
            assert_eq!(got.message, NOT_A_LIST, "{src:?}");
            assert_eq!(got.tier, Tier::Error, "{src:?}");
        }
    }

    #[test]
    fn a_list_of_tasks_is_silent() {
        assert!(judge(ReferenceKind::ImportTasks, "- debug: {msg: ok}\n").is_none());
        assert!(judge(ReferenceKind::IncludeTasks, "- debug: {msg: ok}\n").is_none());
    }

    /// The error is the same for both spellings; only the empty warning's tail differs, since
    /// only the import half is ever reported upstream.
    #[test]
    fn the_two_spellings_differ_only_where_ansible_does() {
        let import = judge(ReferenceKind::ImportTasks, "").unwrap();
        let include = judge(ReferenceKind::IncludeTasks, "").unwrap();
        assert!(import.message.ends_with("Ansible warns and carries on."));
        assert!(include.message.ends_with("Ansible says nothing at all."));
        assert_eq!(
            judge(ReferenceKind::ImportTasks, "a: b\n").unwrap().message,
            judge(ReferenceKind::IncludeTasks, "a: b\n").unwrap().message
        );
    }

    /// Only the two task-file kinds. A role or a playbook import has its own shape rules.
    #[test]
    fn other_reference_kinds_are_not_ours() {
        assert!(judge(ReferenceKind::Role, "a: b\n").is_none());
        assert!(judge(ReferenceKind::ImportPlaybook, "a: b\n").is_none());
    }
}
