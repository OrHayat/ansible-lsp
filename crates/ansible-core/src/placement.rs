//! Placement and mutual-exclusion diagnostics (T-110): structural faults that a per-keyword
//! legal set cannot express. Every key here is spelled correctly and legal where it sits —
//! what is wrong is the shape around it, so [`crate::attributes`] cannot see any of them.
//!
//! Each rule is a shape test on a node and its parent: no resolution, no index, no variables.
//! Measured against ansible-core 2.21.2.
//!
//! This is the play/playbook batch. The task-level rules (loops, `loop_control`, handler
//! placement, the `mod_args` pair) are the later batches of the same ticket.

use crate::keywords;
use crate::parse::{Node, Span};

/// Rule id, for `# noqa: invalid-placement` and for display.
pub const RULE_ID: &str = "invalid-placement";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Problem {
    /// The node the fault is anchored on — the offending key, entry or value.
    pub span: Span,
    pub tier: Tier,
    pub message: String,
}

const HOSTS_EMPTY: &str = "Hosts list cannot be empty. Please check your playbook";
const HOSTS_NONE: &str = "Hosts list cannot contain values of 'None'. Please check your playbook";
const HOSTS_SHAPE: &str = "Hosts list must be a sequence or string. Please check your playbook.";
const NOT_A_PLAY: &str =
    "playbook entries must be either valid plays or 'import_playbook' statements";

/// Every placement problem in the file. `src` is the document text, needed only to quote a
/// bad `hosts:` entry back at the author.
///
/// Playbook-only: a file whose top-level sequence has no `hosts:` and no `import_playbook:`
/// is a task file, where none of these rules exist.
pub fn problems(nodes: &[Node], src: &str) -> Vec<Problem> {
    let mut out = Vec::new();
    // The same document selection `ast::build` makes, so the two agree on what a playbook is.
    let Some(seq) = nodes.iter().find(|n| matches!(n, Node::Sequence { .. })) else {
        return out;
    };
    let items = seq.items();
    let looks_like_plays = items
        .iter()
        .any(|it| keywords::is_play(it.entries().iter().filter_map(|(k, _)| k.as_str())));
    if !looks_like_plays {
        return out;
    }
    for item in items {
        match item {
            Node::Mapping { .. } => play(item, src, &mut out),
            // `if not isinstance(entry, dict)` (`playbook/__init__.py:88-91`). The other
            // three faults at that site — an empty file, a top-level mapping, a list with
            // no plays — need to know the file IS a playbook, which only the command line
            // says. Content alone cannot distinguish an empty playbook from an empty vars
            // file, so they stay unchecked rather than false-positive on every vars file.
            other => out.push(error(other.span(), NOT_A_PLAY.into())),
        }
    }
    out
}

fn error(span: Span, message: String) -> Problem {
    Problem { span, tier: Tier::Error, message }
}

fn play(node: &Node, src: &str, out: &mut Vec<Problem>) {
    // An `import_playbook:` entry is not a Play — it loads as a PlaybookInclude and never
    // reaches any of these checks.
    if node
        .entries()
        .iter()
        .any(|(k, _)| k.as_str().map(keywords::core_action) == Some("import_playbook"))
    {
        return;
    }
    user_and_remote_user(node, out);
    if let Some(hosts) = node.get("hosts") {
        self::hosts(hosts, src, out);
    }
    if let Some(prompts) = node.get("vars_prompt") {
        vars_prompt(prompts, out);
    }
}

/// `preprocess_data` renames `user:` to `remote_user:`, and refuses when the target is
/// already taken (`play.py:166-171`). Anchored on `user:`, the key the message says to drop.
fn user_and_remote_user(node: &Node, out: &mut Vec<Problem>) {
    let key_span = |name: &str| {
        node.entries()
            .iter()
            .rev()
            .find(|(k, _)| k.as_str() == Some(name))
            .map(|(k, _)| k.span())
    };
    if let (Some(user), Some(_)) = (key_span("user"), key_span("remote_user")) {
        out.push(error(
            user,
            "both 'user' and 'remote_user' are set for this play. The use of 'user' is \
             deprecated, and should be removed"
                .into(),
        ));
    }
}

/// `_validate_hosts` (`play.py:120-134`), which runs only when `hosts` was written.
///
/// Known miss, taken deliberately: `hosts: 42` is an int to Ansible and fatal, but the parse
/// tree keeps no scalar style, so it is indistinguishable from the perfectly good
/// `hosts: "42"`. Numbers and booleans therefore pass. A miss, never a false error.
fn hosts(value: &Node, src: &str, out: &mut Vec<Problem>) {
    match value {
        // `hosts:` with nothing after it is None; `hosts: ""` is the empty string. Both are
        // falsy, so both take the same branch and the same message.
        Node::Null { .. } => out.push(error(value.span(), HOSTS_EMPTY.into())),
        Node::Scalar { value: s, span } if s.is_empty() => out.push(error(*span, HOSTS_EMPTY.into())),
        Node::Scalar { .. } => {}
        Node::Sequence { items, span } if items.is_empty() => {
            out.push(error(*span, HOSTS_EMPTY.into()))
        }
        Node::Sequence { items, .. } => {
            for item in items {
                match item {
                    Node::Null { .. } => out.push(error(item.span(), HOSTS_NONE.into())),
                    // Ansible interpolates `str(entry)` — a Python repr we would have to
                    // fake. The source text names the same entry and reads better.
                    Node::Sequence { .. } | Node::Mapping { .. } => out.push(error(
                        item.span(),
                        format!(
                            "Hosts list contains an invalid host value: '{}'",
                            item.span().slice(src)
                        ),
                    )),
                    _ => {}
                }
            }
        }
        Node::Mapping { .. } => out.push(error(value.span(), HOSTS_SHAPE.into())),
        Node::Other { .. } => {}
    }
}

/// `_load_vars_prompt` (`play.py:234-247`). `preprocess_vars` wraps a lone mapping into a
/// one-element list (`vars/manager.py:94-99`), so both spellings are checked the same way.
fn vars_prompt(value: &Node, out: &mut Vec<Problem>) {
    let items: Vec<&Node> = match value {
        // `preprocess_vars(None)` returns None and the loop never runs.
        Node::Null { .. } => return,
        Node::Sequence { items, .. } => items.iter().collect(),
        other => vec![other],
    };
    for item in items {
        // A non-mapping entry dies earlier, in `preprocess_vars` itself, with a different
        // message ("Invalid variable file contents.") — not this rule's to give.
        if !matches!(item, Node::Mapping { .. }) {
            continue;
        }
        if item.get("name").is_none() {
            out.push(error(
                item.span(),
                "Invalid vars_prompt data structure, missing 'name' key".into(),
            ));
        }
        for (k, _) in item.entries() {
            let Some(key) = k.as_str() else { continue };
            if !keywords::VARS_PROMPT_KEYS.contains(&key) {
                out.push(error(
                    k.span(),
                    format!("Invalid vars_prompt data structure, found unsupported key '{key}'"),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    fn check(src: &str) -> Vec<String> {
        let nodes = Document::new(src.to_string()).parse().expect("valid yaml");
        problems(&nodes, src).into_iter().map(|p| p.message).collect()
    }

    /// Row 13. The one genuine mutual exclusion at play level.
    #[test]
    fn user_and_remote_user_together_are_an_error() {
        assert_eq!(
            check("- hosts: web\n  user: alice\n  remote_user: bob\n  tasks: []\n"),
            ["both 'user' and 'remote_user' are set for this play. The use of 'user' is \
              deprecated, and should be removed"]
        );
        // Either one alone is fine — `user:` is renamed, not rejected.
        assert!(check("- hosts: web\n  user: alice\n  tasks: []\n").is_empty());
        assert!(check("- hosts: web\n  remote_user: bob\n  tasks: []\n").is_empty());
    }

    /// Row 13 is anchored on `user:`, since that is the key the message says to remove.
    #[test]
    fn the_user_key_is_what_gets_underlined() {
        let src = "- hosts: web\n  user: alice\n  remote_user: bob\n  tasks: []\n";
        let nodes = Document::new(src.to_string()).parse().unwrap();
        let p = &problems(&nodes, src)[0];
        assert_eq!(p.span.slice(src), "user");
        assert_eq!(p.tier, Tier::Error);
    }

    /// Row 14. Null, empty string and empty list are all falsy, all the same message.
    #[test]
    fn an_empty_hosts_is_an_error_in_all_three_spellings() {
        for src in [
            "- hosts:\n  tasks: []\n",
            "- hosts: \"\"\n  tasks: []\n",
            "- hosts: []\n  tasks: []\n",
        ] {
            assert_eq!(check(src), [HOSTS_EMPTY], "for {src:?}");
        }
    }

    /// Rows 15-17.
    #[test]
    fn hosts_entry_and_container_shapes() {
        assert_eq!(
            check("- hosts:\n    - web\n    -\n    - db\n  tasks: []\n"),
            [HOSTS_NONE]
        );
        assert_eq!(
            check("- hosts:\n    - {name: web}\n  tasks: []\n"),
            ["Hosts list contains an invalid host value: '{name: web}'"]
        );
        assert_eq!(check("- hosts: {group: web}\n  tasks: []\n"), [HOSTS_SHAPE]);
    }

    #[test]
    fn ordinary_hosts_values_stay_silent() {
        for src in [
            "- hosts: all\n  tasks: []\n",
            "- hosts: web:&staging\n  tasks: []\n",
            "- hosts: \"{{ target_group }}\"\n  tasks: []\n",
            "- hosts:\n    - web\n    - db\n  tasks: []\n",
            "- hosts: [web, db]\n  tasks: []\n",
            // An explicitly empty *string* entry is a str to Ansible, not None — it passes.
            "- hosts: [\"\", web]\n  tasks: []\n",
            // No `hosts:` key at all: `_validate_hosts` never runs.
            "- import_playbook: other.yml\n",
        ] {
            assert!(check(src).is_empty(), "for {src:?}: {:?}", check(src));
        }
    }

    /// The documented miss: no scalar style in the tree, so an int is indistinguishable
    /// from a quoted string. Pinned so a future parser change surfaces here.
    #[test]
    fn a_numeric_hosts_is_a_known_miss() {
        assert!(check("- hosts: 42\n  tasks: []\n").is_empty());
    }

    /// Rows 18-19, in both the list and the lone-mapping spelling.
    #[test]
    fn vars_prompt_entries_need_a_name_and_a_known_key() {
        assert_eq!(
            check("- hosts: web\n  vars_prompt:\n    - prompt: Password?\n  tasks: []\n"),
            ["Invalid vars_prompt data structure, missing 'name' key"]
        );
        assert_eq!(
            check(
                "- hosts: web\n  vars_prompt:\n    - name: pw\n      promt: Password?\n  tasks: []\n"
            ),
            ["Invalid vars_prompt data structure, found unsupported key 'promt'"]
        );
        // `preprocess_vars` wraps a lone mapping, so the same faults apply unwrapped.
        assert_eq!(
            check("- hosts: web\n  vars_prompt:\n    prompt: Password?\n  tasks: []\n"),
            ["Invalid vars_prompt data structure, missing 'name' key"]
        );
    }

    #[test]
    fn a_full_legal_vars_prompt_stays_silent() {
        let src = "- hosts: web\n  vars_prompt:\n    - name: pw\n      prompt: Password?\n      \
                   private: true\n      confirm: true\n      encrypt: sha512_crypt\n      \
                   salt_size: 8\n      salt: abc\n      default: x\n      unsafe: true\n  tasks: []\n";
        assert!(check(src).is_empty(), "{:?}", check(src));
        // A null `vars_prompt:` is dropped by `preprocess_vars` before any check.
        assert!(check("- hosts: web\n  vars_prompt:\n  tasks: []\n").is_empty());
        // A `name:` with no value still counts as present — `'name' in prompt_data`.
        assert!(check("- hosts: web\n  vars_prompt:\n    - name:\n  tasks: []\n").is_empty());
    }

    /// Row 8, the half that content alone can decide.
    #[test]
    fn a_playbook_entry_that_is_not_a_mapping_is_an_error() {
        assert_eq!(
            check("- hosts: web\n  tasks: []\n- just-a-string\n"),
            [NOT_A_PLAY]
        );
        assert_eq!(check("- hosts: web\n  tasks: []\n- - nested\n"), [NOT_A_PLAY]);
    }

    /// Task files have none of these rules — every one of them is play-shaped.
    #[test]
    fn a_task_file_is_left_alone() {
        assert!(check("- name: t\n  debug: {msg: hi}\n- command: echo hi\n").is_empty());
        // Including one that would look like a bad `hosts:` if we squinted.
        assert!(check("- name: t\n  add_host:\n    hostname: web\n").is_empty());
    }

    #[test]
    fn a_clean_playbook_has_no_problems() {
        let src = "- name: fine\n  hosts: web\n  remote_user: deploy\n  vars_prompt:\n    \
                   - name: pw\n      prompt: Password?\n  tasks:\n    - debug: {msg: hi}\n\
                   - import_playbook: other.yml\n";
        assert!(check(src).is_empty(), "{:?}", check(src));
    }
}
