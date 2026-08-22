//! T-087: a `vars_files:` item that can never name a file, so the play dies at start.
//!
//! Three shapes fail ansible-core's post-template type gate (`vars/manager.py:348-353`,
//! "A `vars_files` value should either be a string or list of strings"). All three, and
//! the two lookalikes that are *not* fatal, were live-verified on ansible-core 2.21.2 —
//! one play per row, against a control play that ran:
//!
//! | written                  | runtime                                      |
//! | ------------------------ | -------------------------------------------- |
//! | `-`                      | `Invalid `vars_files` value of type 'NoneType'.` |
//! | `- dir: vars/x.yml`      | `Invalid `vars_files` value of type 'dict'.` |
//! | `- - - vars/x.yml`       | `Invalid `vars_files` value of type 'list'.` |
//! | `- -` inside a nested list | `... type 'NoneType'.` — a bad alternative kills the play, it is not skipped |
//! | `vars_files:` with no items | play runs |
//! | `- []`                   | play runs |
//!
//! The last two are why this reads [`crate::ast`] rather than re-walking the nodes: which
//! shapes are entries is already decided in `vars_files_of`, and deciding it twice is how
//! a diagnostic comes to contradict the navigation on the same line (rule 3).
//!
//! Not covered, and deliberately: a non-string *scalar* (`- 5`) is equally fatal
//! (`type 'int'`), but [`crate::parse::Node::Scalar`] keeps no quoting style, so `- 5` and
//! `- "5"` are one value to us and flagging it would fire on correct code.

use crate::ast::{Ast, InvalidVarsFilesEntry, InvalidVarsFilesKind, PlayItem};
use crate::parse::Span;

/// Rule id, for `# noqa: invalid-vars-files-entry` and for display.
pub const RULE_ID: &str = "invalid-vars-files-entry";

#[derive(Debug, Clone)]
pub struct Problem {
    /// The offending item, widened onto its `-` when the item itself is empty.
    pub span: Span,
    pub message: String,
    pub rule: &'static str,
}

/// Every fatal `vars_files:` item in the file. `src` is the document text, used only to
/// give a null item a visible span.
pub fn problems(ast: &Ast, src: &str) -> Vec<Problem> {
    let Ast::Playbook(items) = ast else {
        // `vars_files:` is a play keyword; a task file or a vars file has no plays, and a
        // mapping of that name in one is not this keyword.
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|i| match i {
            PlayItem::Play(p) => Some(&p.invalid_vars_files),
            _ => None,
        })
        .flatten()
        .map(|e| problem(e, src))
        .collect()
}

fn problem(e: &InvalidVarsFilesEntry, src: &str) -> Problem {
    let detail = match e.kind {
        InvalidVarsFilesKind::Null => {
            "this entry has no value, so Ansible reads it as `type 'NoneType'`"
        }
        InvalidVarsFilesKind::Mapping => {
            "a mapping is `type 'dict'`. `vars_files` has no options form — no `dir:`, no \
             `file:`, no regex — all of that is `include_vars` plugin surface this keyword \
             never calls"
        }
        InvalidVarsFilesKind::Nested => {
            "one level of nesting means \"load the first of these that exists\"; a second \
             level is `type 'list'`, which is not a filename"
        }
    };
    Problem {
        span: widen(e, src),
        message: format!(
            "{detail}. A `vars_files` value must be a string or a list of strings, so \
             ansible-core rejects this before the first task and the whole play fails to \
             start. Live-verified on 2.21.2."
        ),
        rule: RULE_ID,
    }
}

/// A null item's span is empty and sits after the `-`, which renders as an invisible
/// zero-width squiggle. Walk back over the spaces to the `-` and cover that instead —
/// it is the only text the mistake has.
fn widen(e: &InvalidVarsFilesEntry, src: &str) -> Span {
    if e.kind != InvalidVarsFilesKind::Null || e.span.start != e.span.end {
        return e.span;
    }
    let before = &src[..e.span.start.min(src.len())];
    match before.trim_end_matches(' ').strip_suffix('-') {
        Some(head) => Span { start: head.len(), end: head.len() + 1 },
        None => e.span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::build;
    use crate::parse::Document;

    fn got(src: &str) -> Vec<Problem> {
        let doc = Document::new(src.to_string());
        let nodes = doc.parse().expect("valid yaml");
        problems(&build(&nodes), &doc.text)
    }

    fn play(entries: &str) -> String {
        format!("- name: p\n  hosts: all\n  vars_files:\n{entries}")
    }

    /// One row per measured-fatal shape, each naming the type ansible-core dies with.
    #[test]
    fn every_fatal_shape_is_reported_with_the_type_ansible_names() {
        for (entries, want) in [
            ("    -\n", "'NoneType'"),
            ("    - dir: vars/x.yml\n", "'dict'"),
            ("    - - - vars/x.yml\n", "'list'"),
            // A bad alternative inside an otherwise fine group: measured fatal, and the
            // good sibling does not rescue it.
            ("    - -\n      - vars/x.yml\n", "'NoneType'"),
            ("    - - dir: vars/x.yml\n", "'dict'"),
        ] {
            let got = got(&play(entries));
            assert_eq!(got.len(), 1, "one problem for {entries:?}: {got:#?}");
            assert!(
                got[0].message.contains(want),
                "{entries:?} must name {want}: {}",
                got[0].message
            );
            assert!(got[0].message.contains("play fails to start"));
            assert_eq!(got[0].rule, RULE_ID);
        }
    }

    /// The controls, and they are the point of the rule: each of these looks like the
    /// shapes above and was measured to RUN. Flagging one would be a false positive on
    /// working Ansible.
    #[test]
    fn the_shapes_that_actually_run_stay_silent() {
        for entries in [
            "    - vars/x.yml\n",
            // One level of nesting is the first-match-wins construct.
            "    - - vars/a.yml\n      - vars/b.yml\n",
            // Nothing in it to fail the gate.
            "    - []\n",
            // An explicitly empty string passes the type gate — it is a string. What it
            // then resolves to is the resolver's business, not this rule's.
            "    - ''\n",
        ] {
            assert!(got(&play(entries)).is_empty(), "must stay silent: {entries:?}");
        }
        // A null `vars_files:` key, and the bare-string shorthand.
        assert!(got("- name: p\n  hosts: all\n  vars_files:\n").is_empty());
        assert!(got("- name: p\n  hosts: all\n  vars_files: vars/x.yml\n").is_empty());
    }

    /// A zero-width range is an invisible squiggle, so a null item claims its `-`.
    #[test]
    fn a_null_item_is_anchored_on_its_dash() {
        let src = play("    -\n");
        let got = got(&src);
        assert_eq!(got.len(), 1);
        assert_eq!(&src[got[0].span.start..got[0].span.end], "-");
    }

    /// Every play in the file is walked, not just the first.
    #[test]
    fn a_later_play_is_reported_too() {
        let src = format!("{}\n{}", play("    - vars/x.yml\n"), play("    - dir: x\n"));
        assert_eq!(got(&src).len(), 1);
    }

    /// Not a playbook: a task file or a vars file has no plays, and a mapping that
    /// happens to be called `vars_files` in one is not this keyword.
    #[test]
    fn a_non_playbook_is_never_walked() {
        assert!(got("- name: t\n  ansible.builtin.debug:\n    msg: hi\n").is_empty());
        assert!(got("vars_files:\n  - dir: x\n").is_empty());
    }
}
