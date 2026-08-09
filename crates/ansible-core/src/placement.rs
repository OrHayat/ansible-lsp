//! Placement and mutual-exclusion diagnostics (T-110): structural faults that a per-keyword
//! legal set cannot express. Every key here is spelled correctly and legal where it sits —
//! what is wrong is the shape around it, so [`crate::attributes`] cannot see any of them.
//!
//! Each rule is a shape test on a node and its parent: no resolution, no index, no variables.
//! Measured against ansible-core 2.21.2.
//!
//! Shipped so far: the play/playbook batch, and the two loop-on-import rules. The rest of
//! the task-level rules (`loop_control`, handler placement, the `mod_args` pair) are the
//! later batches of the same ticket.

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
/// The play-shaped rules apply only to playbooks; the task-shaped ones run in both, since a
/// role's `tasks/main.yml` reaches the same `load_list_of_tasks` that a play's `tasks:` does.
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
        // A standalone task file: `tasks/main.yml`, a handler file, an include target.
        for item in items {
            stmt(item, &mut out);
        }
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

/// One entry of a task list: a block, whose three task-holding keys recurse, or a task.
fn stmt(node: &Node, out: &mut Vec<Problem>) {
    if !matches!(node, Node::Mapping { .. }) {
        return;
    }
    if node.get("block").is_some() {
        for key in keywords::BLOCK_TASK_CONTAINERS {
            if let Some(list) = node.get(key) {
                for child in list.items() {
                    stmt(child, out);
                }
            }
        }
        return;
    }
    loop_on_import(node, out);
}

/// Rows 3 and 4. `import_*` is expanded at parse time, before any loop could iterate, so a
/// loop on one is fatal — `task.loop is not None` after `preprocess_data` has folded every
/// `with_*` into `loop` (`helpers.py:152-154`, `helpers.py:258-260`).
///
/// Both messages are literal strings upstream, so an FQCN spelling still reports the bare
/// name — live-verified. `action: import_tasks` is a miss: the module is read from the
/// written key only, and that spelling is vanishingly rare for an import.
fn loop_on_import(node: &Node, out: &mut Vec<Problem>) {
    let import = node.entries().iter().find_map(|(k, _)| {
        match keywords::core_action(k.as_str()?) {
            "import_tasks" => Some(("import_tasks", "include_tasks")),
            "import_role" => Some(("import_role", "include_role")),
            _ => None,
        }
    });
    let Some((action, replacement)) = import else { return };
    let Some(key) = live_loop(node) else { return };
    out.push(error(
        key,
        format!(
            "You cannot use loops on '{action}' statements. You should use '{replacement}' \
             instead."
        ),
    ));
}

/// The span of a loop key that would leave `task.loop` set. A `loop:` written with no value
/// is `None` and passes — live-verified, `loop: []` is empty but not None and still fails.
/// A `with_*` with no value dies earlier, in `preprocess_data`, with a different message
/// (row 20), so it is not this rule's to report.
///
/// The `with_` prefix is matched wholesale, which covers every documented lookup loop —
/// all fourteen measured — but over-reaches in one case: Ansible only folds `with_x` into
/// `loop` when `x` names an *installed* lookup (`task.py:336`), and an unrecognised one
/// falls through to `'with_frobnicate' is not a valid attribute` instead. So a typo'd
/// `with_item` on an import gets our loop message where Ansible gives an invalid-attribute
/// one. Both are errors on the same line; only the reason differs. Enumerating lookups is
/// T-115, and it is the same leniency `keywords::is_task_directive` already takes.
fn live_loop(node: &Node) -> Option<Span> {
    node.entries().iter().find_map(|(k, v)| {
        let key = k.as_str()?;
        let is_loop = key == "loop" || key.starts_with("with_");
        (is_loop && !matches!(v, Node::Null { .. })).then(|| k.span())
    })
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
    // `pre_tasks`, `tasks`, `post_tasks` and `handlers` all reach the same
    // `load_list_of_tasks`, so the task-shaped rules apply identically in each.
    for key in keywords::PLAY_TASK_CONTAINERS {
        if let Some(list) = node.get(key) {
            for item in list.items() {
                stmt(item, out);
            }
        }
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

    /// Task files have none of the *play* rules — every one of them is play-shaped.
    #[test]
    fn a_task_file_is_left_alone_by_the_play_rules() {
        assert!(check("- name: t\n  debug: {msg: hi}\n- command: echo hi\n").is_empty());
        // Including one that would look like a bad `hosts:` if we squinted.
        assert!(check("- name: t\n  add_host:\n    hostname: web\n").is_empty());
    }

    const NO_LOOP_TASKS: &str =
        "You cannot use loops on 'import_tasks' statements. You should use 'include_tasks' \
         instead.";
    const NO_LOOP_ROLE: &str =
        "You cannot use loops on 'import_role' statements. You should use 'include_role' \
         instead.";

    /// Rows 3 and 4. `with_*` counts because `preprocess_data` folds it into `loop` before
    /// the check runs — all four spellings measured on 2.21.2.
    #[test]
    fn a_loop_on_an_import_is_an_error() {
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - import_tasks: f.yml\n      loop: [1, 2]\n"),
            [NO_LOOP_TASKS]
        );
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - import_tasks: f.yml\n      with_items: [1]\n"),
            [NO_LOOP_TASKS]
        );
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - import_role: {name: r}\n      loop: [1]\n"),
            [NO_LOOP_ROLE]
        );
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - import_role: {name: r}\n      with_items: [1]\n"),
            [NO_LOOP_ROLE]
        );
    }

    /// Every documented lookup loop, measured — the prefix match needs no list.
    #[test]
    fn every_with_lookup_spelling_counts_as_a_loop() {
        for k in [
            "with_list",
            "with_items",
            "with_indexed_items",
            "with_flattened",
            "with_together",
            "with_dict",
            "with_sequence",
            "with_subelements",
            "with_nested",
            "with_cartesian",
            "with_random_choice",
            "with_fileglob",
            "with_first_found",
            "with_lines",
        ] {
            assert_eq!(
                check(&format!(
                    "- hosts: web\n  tasks:\n    - import_tasks: f.yml\n      {k}: [1]\n"
                )),
                [NO_LOOP_TASKS],
                "for {k}"
            );
        }
    }

    /// The documented over-reach: Ansible folds `with_x` into `loop` only for an installed
    /// lookup, so `with_frobnicate` is an invalid attribute to it and a loop to us. Same
    /// line, same severity, different reason. Pinned so T-115 can tighten it.
    #[test]
    fn an_unknown_with_lookup_is_reported_as_a_loop_not_an_unknown_key() {
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - import_tasks: f.yml\n      with_frobnicate: [1]\n"),
            [NO_LOOP_TASKS],
            "upstream says: 'with_frobnicate' is not a valid attribute for a TaskInclude"
        );
    }

    /// The message is a literal upstream, so an FQCN import still reports the bare name.
    #[test]
    fn an_fqcn_import_reports_the_bare_action_name() {
        for prefix in ["ansible.builtin.", "ansible.legacy."] {
            assert_eq!(
                check(&format!(
                    "- hosts: web\n  tasks:\n    - {prefix}import_tasks: f.yml\n      loop: [1]\n"
                )),
                [NO_LOOP_TASKS]
            );
        }
    }

    /// `task.loop is not None`, so a `loop:` with no value passes — but an empty list does
    /// not. Both live-verified.
    #[test]
    fn a_null_loop_passes_and_an_empty_list_does_not() {
        assert!(check("- hosts: web\n  tasks:\n    - import_tasks: f.yml\n      loop:\n").is_empty());
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - import_tasks: f.yml\n      loop: []\n"),
            [NO_LOOP_TASKS]
        );
    }

    /// The dynamic twins are exactly what the message tells you to switch to.
    #[test]
    fn loops_on_the_include_twins_stay_silent() {
        assert!(check("- hosts: web\n  tasks:\n    - include_tasks: f.yml\n      loop: [1]\n").is_empty());
        assert!(check("- hosts: web\n  tasks:\n    - include_role: {name: r}\n      loop: [1]\n").is_empty());
        // And an import with no loop at all.
        assert!(check("- hosts: web\n  tasks:\n    - import_tasks: f.yml\n").is_empty());
    }

    /// Every task list reaches the same `load_list_of_tasks`: all four play containers,
    /// nested blocks, and a standalone task file.
    #[test]
    fn the_loop_rule_reaches_every_task_list() {
        for key in ["pre_tasks", "tasks", "post_tasks", "handlers"] {
            assert_eq!(
                check(&format!(
                    "- hosts: web\n  {key}:\n    - import_tasks: f.yml\n      loop: [1]\n"
                )),
                [NO_LOOP_TASKS],
                "in {key}"
            );
        }
        for key in ["block", "rescue", "always"] {
            assert_eq!(
                check(&format!(
                    "- hosts: web\n  tasks:\n    - block: [{{debug: null}}]\n      \
                     {key}:\n        - import_tasks: f.yml\n          loop: [1]\n"
                )),
                [NO_LOOP_TASKS],
                "in {key}"
            );
        }
        // A role's tasks/main.yml is not a playbook, but it is the same task list.
        assert_eq!(
            check("- import_tasks: f.yml\n  loop: [1]\n"),
            [NO_LOOP_TASKS]
        );
    }

    /// Anchored on the loop key, where the fix goes — Ansible anchors on the whole task.
    #[test]
    fn the_loop_key_is_what_gets_underlined() {
        let src = "- hosts: web\n  tasks:\n    - import_tasks: f.yml\n      with_items: [1]\n";
        let nodes = Document::new(src.to_string()).parse().unwrap();
        assert_eq!(problems(&nodes, src)[0].span.slice(src), "with_items");
    }

    #[test]
    fn a_clean_playbook_has_no_problems() {
        let src = "- name: fine\n  hosts: web\n  remote_user: deploy\n  vars_prompt:\n    \
                   - name: pw\n      prompt: Password?\n  tasks:\n    - debug: {msg: hi}\n\
                   - import_playbook: other.yml\n";
        assert!(check(src).is_empty(), "{:?}", check(src));
    }
}
