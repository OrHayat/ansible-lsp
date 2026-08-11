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
use crate::references::{Reference, ReferenceKind};

/// Row 5. Verbatim upstream, and identical for both spellings — only the timing differs.
pub const NOT_A_LIST_RULE_ID: &str = "invalid-task-file";
/// Row 23. Ours for `include_tasks`, which has no upstream message, so one id covers both.
pub const EMPTY_RULE_ID: &str = "empty-task-file";
/// Row 8, the import half: the target holds no plays. Two upstream messages, one fault.
pub const EMPTY_PLAYBOOK_RULE_ID: &str = "empty-playbook";
/// Row 8, the import half: the target is not a list at all.
pub const NOT_A_PLAYBOOK_RULE_ID: &str = "invalid-playbook";

const NOT_A_LIST: &str = "included task files must contain a list of tasks";
const EMPTY_PLAYBOOK: &str = "Empty playbook, nothing to do";
const NO_PLAYS: &str = "A playbook must contain at least one play";
const NOT_A_PLAYBOOK: &str = "A playbook must be a list of plays";

/// The fault in `target`, anchored on `span` — the reference in the file being edited, not a
/// position in the file at fault, which may not even be open.
///
/// `target` is every document libyaml found. Ansible reads one (`get_single_data`), so only
/// the first is judged.
pub fn problem(r: &Reference, target: &[Node]) -> Option<Problem> {
    let span = r.span;
    let verb = match r.kind {
        ReferenceKind::ImportTasks => "imports",
        ReferenceKind::IncludeTasks => "includes",
        // Only a real playbook-level entry opens its target as a playbook. The same key in a
        // task list is row `ip`'s fault and never reads the file, so judging it would invent a
        // second fault for one mistake.
        ReferenceKind::ImportPlaybook if r.playbook_entry => return playbook(target, span),
        _ => return None,
    };
    let root = target.first();
    if falsy(root) {
        // Ansible's own warning names the resolved path. Ours anchors on the reference, where
        // the path is already written, so it says what the author gains from knowing instead:
        // that this line does nothing, and whether they would ever have been told.
        let tail = match r.kind {
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

/// Row 8 for a file reached by `import_playbook:` (`playbook/__init__.py:74-91`).
///
/// The branch order is **not** the task-file one and does not share [`falsy`], which is the
/// thing to get right here. A playbook is tested for `ds is None` first and falsiness only
/// last, so an empty **mapping** is "not a list of plays" while an empty **list** is "no
/// plays" — where a task file calls both of them empty. All four shapes measured on 2.21.2.
///
/// Both messages keep upstream's opening sentence and drop its interpolation: the path it
/// appends is the one already written on the line we anchor to, and the `<class ...>` in the
/// third is a Python type name with no meaning to someone reading YAML.
fn playbook(target: &[Node], span: Span) -> Option<Problem> {
    let (message, rule) = match target.first() {
        // `ds is None` — an empty file, or one holding only comments.
        None | Some(Node::Null { .. }) => (EMPTY_PLAYBOOK, EMPTY_PLAYBOOK_RULE_ID),
        Some(Node::Sequence { items, .. }) if items.is_empty() => (NO_PLAYS, EMPTY_PLAYBOOK_RULE_ID),
        Some(Node::Sequence { .. }) => return None,
        // An alias at the document root resolves to whatever the anchor holds. T-160.
        Some(Node::Other { .. }) => return None,
        _ => (NOT_A_PLAYBOOK, NOT_A_PLAYBOOK_RULE_ID),
    };
    Some(Problem { span, tier: Tier::Error, message: message.into(), rule })
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
        let mut r = Reference::new(kind, "t.yml", Span { start: 0, end: 1 });
        r.playbook_entry = true;
        let nodes = Document::new(src.to_string()).parse().expect("target parses");
        problem(&r, &nodes)
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

    #[test]
    fn other_reference_kinds_are_not_ours() {
        assert!(judge(ReferenceKind::Role, "a: b\n").is_none());
        assert!(judge(ReferenceKind::IncludeVarsDir, "a: b\n").is_none());
    }

    /// Row 8's import half. The split between the two messages is the point: a playbook is
    /// tested for `None` first and falsiness last, so `{}` and `[]` land on different rules —
    /// where a task file calls both of them empty. Each measured on 2.21.2.
    #[test]
    fn a_playbook_target_is_judged_on_ansibles_own_branch_order() {
        let cases = [
            ("", EMPTY_PLAYBOOK, EMPTY_PLAYBOOK_RULE_ID),
            ("# only a comment\n", EMPTY_PLAYBOOK, EMPTY_PLAYBOOK_RULE_ID),
            ("[]\n", NO_PLAYS, EMPTY_PLAYBOOK_RULE_ID),
            // Falsy, but a mapping — so the *shape* complaint, not the empty one.
            ("{}\n", NOT_A_PLAYBOOK, NOT_A_PLAYBOOK_RULE_ID),
            ("a: b\n", NOT_A_PLAYBOOK, NOT_A_PLAYBOOK_RULE_ID),
            ("hello\n", NOT_A_PLAYBOOK, NOT_A_PLAYBOOK_RULE_ID),
        ];
        for (src, message, rule) in cases {
            let got = judge(ReferenceKind::ImportPlaybook, src).expect("a problem");
            assert_eq!((got.message.as_str(), got.rule), (message, rule), "{src:?}");
            assert_eq!(got.tier, Tier::Error, "{src:?}");
        }
        assert!(judge(ReferenceKind::ImportPlaybook, "- hosts: web\n  tasks: []\n").is_none());
    }

    /// The same key inside a task list never opens its target — row `ip` owns that mistake.
    #[test]
    fn a_task_level_import_playbook_has_no_target_to_judge() {
        let r = Reference::new(ReferenceKind::ImportPlaybook, "t.yml", Span { start: 0, end: 1 });
        assert!(!r.playbook_entry);
        let nodes = Document::new(String::new()).parse().unwrap();
        assert!(problem(&r, &nodes).is_none());
    }
}
