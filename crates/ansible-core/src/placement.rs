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

/// Rule id, for `# noqa: invalid-placement` and for display. Everything under it replicates
/// a shape ansible-core itself refuses.
pub const RULE_ID: &str = "invalid-placement";

/// A second rule id, for the one place this module deliberately speaks where ansible-core
/// stays silent: a loop keyword whose value is discarded while its lookup still applies.
/// Separate so it can be suppressed and toggled without touching the replication rules.
pub const SHADOWED_LOOP_RULE_ID: &str = "shadowed-loop";

/// T-155. Also ours, not ansible-core's: a `loop_control:` with no loop to control. Every
/// key in it is inert, and Ansible runs the task without a murmur.
pub const DEAD_LOOP_CONTROL_RULE_ID: &str = "dead-loop-control";

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
    /// Which rule fired — [`RULE_ID`] for everything that replicates ansible-core,
    /// [`SHADOWED_LOOP_RULE_ID`] for the one divergence.
    pub rule: &'static str,
}

const HOSTS_EMPTY: &str = "Hosts list cannot be empty. Please check your playbook";
const HOSTS_NONE: &str = "Hosts list cannot contain values of 'None'. Please check your playbook";
const HOSTS_SHAPE: &str = "Hosts list must be a sequence or string. Please check your playbook.";
const NOT_A_PLAY: &str =
    "playbook entries must be either valid plays or 'import_playbook' statements";
const LOOP_CONTROL_SHAPE: &str = "the `loop_control` value must be specified as a dictionary \
                                  and cannot be a variable itself (though it can contain \
                                  variables)";

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
    // `preprocess_data` runs inside `Task.load`, so a duplicate loop — or a `with_*` with no
    // value — is raised before the field loaders run and long before `helpers.py` asks what
    // the action was. One fault, one message, in Ansible's own order.
    if duplicate_loop(node, out) {
        return;
    }
    loop_control_checks(node, out);
    loop_on_import(node, out);
}

/// The span of a task's key, for anchoring. Searched from the end, like [`Node::get`]: on a
/// duplicate key Ansible keeps the last, so that is the one a diagnostic should point at.
fn key_span(node: &Node, name: &str) -> Option<Span> {
    node.entries()
        .iter()
        .rev()
        .find(|(k, _)| k.as_str() == Some(name))
        .map(|(k, _)| k.span())
}

/// Row 21, and T-155 riding on the same read of the key.
///
/// Row 21 is `_load_loop_control` (`task.py:346-352`), and it is stricter than it looks:
/// measured on 2.21.2, a valueless `loop_control:` is **fatal** — unlike `loop:`, where the
/// guard is `is not None` — and so is a templated scalar, which is what the message's "cannot
/// be a variable itself" is about. Anything that is not a mapping fails, loop or no loop.
///
/// T-155 is ours: a well-formed `loop_control:` on a task with no loop is inert, and Ansible
/// runs it clean, exit 0, no warning. Blocks are excluded — `loop_control` is not a Block
/// keyword at all, so `'loop_control' is not a valid attribute for a Block` is T-107's to
/// give, and `stmt` never reaches here for one.
fn loop_control_checks(node: &Node, out: &mut Vec<Problem>) {
    let Some(lc) = node.get("loop_control") else { return };
    let Some(anchor) = key_span(node, "loop_control") else { return };
    if !matches!(lc, Node::Mapping { .. }) {
        out.push(error(anchor, LOOP_CONTROL_SHAPE.into()));
        return;
    }
    if live_loop(node).is_some() {
        return;
    }
    let keys: Vec<&str> = lc.entries().iter().filter_map(|(k, _)| k.as_str()).collect();
    if keys.is_empty() {
        return;
    }
    let (list, verb) = match keys.len() {
        1 => (format!("`{}`", keys[0]), "has"),
        _ => (
            keys.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", "),
            "have",
        ),
    };
    out.push(Problem {
        span: anchor,
        tier: Tier::Warning,
        message: format!(
            "`loop_control:` has no loop to control — this task has no `loop:` and no \
             `with_*`, so {list} {verb} no effect. Ansible runs this silently. Add the loop, \
             or delete the block."
        ),
        rule: DEAD_LOOP_CONTROL_RULE_ID,
    });
}

/// Row 10. `_preprocess_with_loop` refuses a `with_*` when `loop`/`loop_with` is **already**
/// set (`task.py:252-261`), and `preprocess_data` walks the task's keys in written order —
/// so this reads the keys in order and only fires where Ansible does. Returns whether it did.
///
/// Row 20 lives here too, since it is the next line of the same function: a `with_*` written
/// with no value at all.
///
/// That ordering makes the rule asymmetric, which is measured, not assumed:
/// `loop:` then `with_items:` is fatal, while `with_items:` then `loop:` runs clean — and
/// runs *wrong*. See `upstream/ansible-duplicate-loop.md`. The silent order gets
/// [`SHADOWED_LOOP_RULE_ID`], a warning of our own rather than a borrowed error.
///
/// Returns whether a fatal duplicate was reported, which suppresses the import rule — the
/// shadowed-loop warning does not, since on an import both faults are real and Ansible does
/// raise the import one.
fn duplicate_loop(node: &Node, out: &mut Vec<Problem>) -> bool {
    let mut loop_set = false;
    // A `with_*` that a later `loop:` would overwrite the value of, without clearing the
    // lookup it registered.
    let mut shadowable: Option<(&str, Span)> = None;
    for (k, v) in node.entries() {
        let Some(key) = k.as_str() else { continue };
        if key == "loop" {
            // `new_ds['loop'] = v` goes through the plain attribute branch, and the guard
            // is `is not None` — so a `loop:` with no value never counts as a loop.
            if matches!(v, Node::Null { .. }) {
                continue;
            }
            if let Some((lookup, span)) = shadowable.take() {
                out.push(shadowed_loop(lookup, span));
                return false;
            }
            loop_set = true;
        } else if let Some(lookup) = key.strip_prefix("with_") {
            if loop_set {
                out.push(error(k.span(), format!("duplicate loop in task: {lookup}")));
                return true;
            }
            // Row 20, raised by the same function one line after the duplicate check —
            // which is why the duplicate wins when both apply. Only a *missing* value
            // counts: measured, `with_items: ""` and `with_items: []` both run clean, so
            // the empty-string and empty-list spellings must stay silent.
            if matches!(v, Node::Null { .. }) {
                out.push(error(
                    k.span(),
                    format!("you must specify a value when using {key}"),
                ));
                return true;
            }
            loop_set = true;
            shadowable = Some((lookup, k.span()));
        }
    }
    false
}

/// Our own warning, with no upstream counterpart: `_preprocess_with_loop` records **two**
/// keys, `loop_with` and `loop` (`task.py:260-261`), and a later `loop:` overwrites only
/// `loop`. The stale `loop_with` still picks the plugin at run time
/// (`task_executor.py:157-170`), so the task loops with a lookup whose own value was thrown
/// away — different iterations from the same `loop:` written alone, and Ansible says nothing.
fn shadowed_loop(lookup: &str, span: Span) -> Problem {
    Problem {
        span,
        tier: Tier::Warning,
        message: format!(
            "`with_{lookup}:` is overridden by the `loop:` below it — its value is discarded, \
             but the '{lookup}' lookup it registered is not, so this task loops with \
             '{lookup}' over the `loop:` value and iterates differently from the same \
             `loop:` written alone. Ansible accepts this silently (it is fatal in the other \
             order). Delete one of the two keywords."
        ),
        rule: SHADOWED_LOOP_RULE_ID,
    }
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
    Problem { span, tier: Tier::Error, message, rule: RULE_ID }
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

    /// Row 10, in the order that actually raises. Every case measured on 2.21.2.
    #[test]
    fn a_second_loop_keyword_is_a_duplicate_loop() {
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - debug:\n      loop: [1]\n      with_items: [a]\n"),
            ["duplicate loop in task: items"]
        );
        // Two `with_*` are symmetric: whichever is second raises, naming itself.
        assert_eq!(
            check(
                "- hosts: web\n  tasks:\n    - debug:\n      with_items: [a]\n      with_list: [b]\n"
            ),
            ["duplicate loop in task: list"]
        );
        assert_eq!(
            check(
                "- hosts: web\n  tasks:\n    - debug:\n      with_list: [b]\n      with_items: [a]\n"
            ),
            ["duplicate loop in task: items"]
        );
    }

    /// The order Ansible accepts gets a warning of our own, on its own rule id — the
    /// discarded `with_*` still steers the surviving `loop:`
    /// (`upstream/ansible-duplicate-loop.md`).
    #[test]
    fn a_loop_written_after_a_with_star_warns_on_its_own_rule() {
        let src = "- hosts: web\n  tasks:\n    - debug:\n      with_items: [a]\n      loop: [1]\n";
        let nodes = Document::new(src.to_string()).parse().unwrap();
        let got = problems(&nodes, src);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].tier, Tier::Warning, "Ansible runs this, so it is not an error");
        assert_eq!(got[0].rule, SHADOWED_LOOP_RULE_ID);
        // Anchored on the dead keyword, which is the line to delete.
        assert_eq!(got[0].span.slice(src), "with_items");
        for want in ["with_items:", "discarded", "'items' lookup", "other\norder", "Delete one"] {
            let want = want.replace('\n', " ");
            assert!(got[0].message.contains(&want), "missing {want:?}: {}", got[0].message);
        }
    }

    /// On an import, both faults are real and Ansible does raise the import one, so the
    /// warning does not suppress it.
    #[test]
    fn a_shadowed_loop_on_an_import_reports_both() {
        let got = check(
            "- hosts: web\n  tasks:\n    - import_tasks: f.yml\n      with_items: [a]\n      \
             loop: [1]\n",
        );
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].contains("overridden by the `loop:`"));
        assert_eq!(got[1], NO_LOOP_TASKS);
    }

    /// The guard is `is not None`, so a valueless `loop:` never counts as the first loop.
    #[test]
    fn a_null_loop_does_not_make_a_following_with_star_a_duplicate() {
        assert!(check(
            "- hosts: web\n  tasks:\n    - debug:\n      loop:\n      with_items: [a]\n"
        )
        .is_empty());
    }

    /// Row 20. Only a *missing* value counts — measured, the empty spellings run clean.
    #[test]
    fn a_with_star_written_with_no_value_is_an_error() {
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - debug:\n      with_items:\n"),
            ["you must specify a value when using with_items"]
        );
        // The message names the key as written, not the lookup.
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - debug:\n      with_dict:\n"),
            ["you must specify a value when using with_dict"]
        );
        for empty in ["\"\"", "[]", "{}"] {
            assert!(
                check(&format!(
                    "- hosts: web\n  tasks:\n    - debug:\n      with_items: {empty}\n"
                ))
                .is_empty(),
                "with_items: {empty} runs clean upstream"
            );
        }
    }

    /// The duplicate check runs one line before the null-value check in the same function,
    /// so it wins — measured: `loop:` + a null `with_items:` is a duplicate, not row 20.
    #[test]
    fn the_duplicate_check_beats_the_null_value_check() {
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - debug:\n      loop: [1]\n      with_items:\n"),
            ["duplicate loop in task: items"]
        );
        // The other way round the null check wins, since no loop was recorded yet. A null
        // `with_*` never registered a lookup, so there is nothing to shadow either.
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - debug:\n      with_items:\n      loop: [1]\n"),
            ["you must specify a value when using with_items"]
        );
    }

    /// `preprocess_data` runs inside `Task.load`, before `helpers.py` looks at the action,
    /// so a duplicate loop on an import reports the duplicate — one fault, one message.
    #[test]
    fn a_duplicate_loop_on_an_import_beats_the_import_rule() {
        assert_eq!(
            check(
                "- hosts: web\n  tasks:\n    - import_tasks: f.yml\n      loop: [1]\n      \
                 with_items: [a]\n"
            ),
            ["duplicate loop in task: items"]
        );
    }

    /// Row 21. Stricter than `loop:`: a valueless `loop_control:` is fatal too, and so is a
    /// templated scalar — all measured on 2.21.2, with and without a loop.
    #[test]
    fn a_loop_control_that_is_not_a_mapping_is_an_error() {
        for value in ["nonsense", "[a, b]", "", "\"{{ a_var }}\""] {
            assert_eq!(
                check(&format!(
                    "- hosts: web\n  tasks:\n    - debug:\n      loop: [1]\n      \
                     loop_control: {value}\n"
                )),
                [LOOP_CONTROL_SHAPE],
                "for loop_control: {value:?}"
            );
        }
        // No loop needed — the field loader runs either way.
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - debug:\n      loop_control: nonsense\n"),
            [LOOP_CONTROL_SHAPE]
        );
    }

    /// T-155: a well-formed `loop_control:` with no loop to control. Ansible runs it clean,
    /// exit 0, no warning — measured — so this is ours, on its own rule id.
    #[test]
    fn a_loop_control_with_no_loop_warns_and_names_the_inert_keys() {
        let src = "- hosts: web\n  tasks:\n    - debug:\n      loop_control:\n        \
                   loop_var: it\n        label: x\n";
        let nodes = Document::new(src.to_string()).parse().unwrap();
        let got = problems(&nodes, src);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].tier, Tier::Warning);
        assert_eq!(got[0].rule, DEAD_LOOP_CONTROL_RULE_ID);
        assert_eq!(got[0].span.slice(src), "loop_control");
        assert!(got[0].message.contains("`loop_var`, `label` have no effect"), "{}", got[0].message);
        // One key gets the singular verb.
        let one = "- hosts: web\n  tasks:\n    - debug:\n      loop_control: {loop_var: it}\n";
        let nodes = Document::new(one.to_string()).parse().unwrap();
        assert!(problems(&nodes, one)[0].message.contains("`loop_var` has no effect"));
    }

    /// T-155 reads the effective task, so the include and import actions count too — all
    /// measured clean upstream. A `with_*` counts as the loop just as `loop:` does.
    #[test]
    fn the_dead_loop_control_rule_covers_every_task_shape() {
        for action in [
            "debug:",
            "include_tasks: f.yml",
            "include_role: {name: r}",
            "import_tasks: f.yml",
        ] {
            let got = check(&format!(
                "- hosts: web\n  tasks:\n    - {action}\n      loop_control: {{loop_var: it}}\n"
            ));
            assert_eq!(got.len(), 1, "for {action}: {got:?}");
            assert!(got[0].contains("no loop to control"));
        }
        // With a loop of either spelling, silence.
        for loop_key in ["loop: [1]", "with_items: [a]"] {
            assert!(
                check(&format!(
                    "- hosts: web\n  tasks:\n    - debug:\n      {loop_key}\n      \
                     loop_control: {{loop_var: it}}\n"
                ))
                .is_empty(),
                "for {loop_key}"
            );
        }
        // A valueless `loop:` is not a loop, so the block really is dead.
        assert_eq!(
            check(
                "- hosts: web\n  tasks:\n    - debug:\n      loop:\n      \
                 loop_control: {loop_var: it}\n"
            )
            .len(),
            1
        );
    }

    /// On a Block, `loop_control` is not a keyword at all — T-107 gives
    /// `'loop_control' is not a valid attribute for a Block`, measured, so this rule must
    /// not also speak.
    #[test]
    fn a_block_loop_control_is_left_to_the_keyword_rule() {
        assert!(check(
            "- hosts: web\n  tasks:\n    - block:\n        - debug:\n      \
             loop_control: {loop_var: it}\n"
        )
        .is_empty());
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
