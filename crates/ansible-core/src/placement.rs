//! Placement and mutual-exclusion diagnostics (T-110): structural faults that a per-keyword
//! legal set cannot express. Every key here is spelled correctly and legal where it sits —
//! what is wrong is the shape around it, so [`crate::attributes`] cannot see any of them.
//!
//! Each rule is a shape test on a node and its parent: no resolution, no index, no variables.
//! Measured against ansible-core 2.21.2.
//!
//! The rules fall into three shapes, and only the first is data: *position* rules, which turn
//! on where a statement sits and live in [`REFUSED`]; *co-occurrence* rules, which turn on two
//! keys of one node; and per-key value rules. The file-level shapes and the `mod_args` pair are
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

/// Row 24, and ours for the same reason: `local_action` overwrites an explicit `delegate_to`
/// (`mod_args.py:303,325`) and the task quietly runs somewhere else than the author wrote.
pub const DISCARDED_DELEGATE_TO_RULE_ID: &str = "discarded-delegate-to";

/// `import_playbook:` written inside a task list. Ansible does fail, but only at run time and
/// with a message about *parameters* — so the message here is ours, and needs its own id.
pub const MISPLACED_IMPORT_PLAYBOOK_RULE_ID: &str = "misplaced-import-playbook";

/// A task-list entry that is not a mapping. Fatal upstream, but the message names the wrong
/// value and the wrong type and carries no position at all, so ours replaces it rather than
/// quoting it — see `upstream/ansible-malformed-task-entry.md`.
pub const MALFORMED_TASK_ENTRY_RULE_ID: &str = "malformed-task-entry";

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
const BLOCK_AS_HANDLER: &str = "Using a block as a handler is not supported.";
const NO_MODULE: &str = "no module/action detected in task.";
const USER_AND_REMOTE_USER: &str = "both 'user' and 'remote_user' are set for this play. The \
                                    use of 'user' is deprecated, and should be removed";
const ACTION_AND_LOCAL_ACTION: &str = "action and local_action are mutually exclusive";
const DISCARDED_DELEGATE_TO: &str = "`local_action` already delegates to localhost, so this \
                                     `delegate_to` is discarded — the task runs locally, not \
                                     on the host named here. Use `delegate_to` with a plain \
                                     module instead, or drop this key.";
const END_ROLE_HANDLER: &str = "Cannot execute 'end_role' from a handler";
const END_ROLE_OUTSIDE: &str = "Cannot execute 'end_role' from outside of a role";
const FLUSH_AS_HANDLER: &str = "flush_handlers cannot be used as a handler";
const NOT_A_MAPPING: &str = "every entry in a task list must be a mapping — a task, or a \
                             `block:`. Ansible refuses to load this file.";
/// Verbatim, wording included. `preprocess_vars` is shared with vars-file loading
/// (`vars/manager.py:357`) and only that caller has a file, but replicating the message is what
/// makes it greppable — and unlike [`NOT_A_MAPPING`] it is a real error with a real position.
const NOT_VARS_PROMPT_DATA: &str = "Invalid variable file contents.";
const IMPORT_PLAYBOOK_IN_TASKS: &str = "`import_playbook` is only valid as a top-level playbook \
                                        entry. Here it is parsed as a module and fails at run \
                                        time. Use `import_tasks:` to pull in a task file, or \
                                        move this out of the task list.";

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
        // A standalone task file, and every position rule is a documented miss here.
        //
        // Two different unknowns, neither answerable from content. Whether this is a handler
        // file: a role's `handlers/main.yml` fires rows 1, 2, 6a and 25 upstream, but it reads
        // exactly like `tasks/main.yml`, where the same nesting is legal and common. And
        // whether we are in a role, for row 6b — measured, a byte-identical include target is
        // legal when a role includes it and fatal when a play does, so the *file* has no answer
        // at all. The first wants T-150's file-kind matrix; the second wants T-020's reverse
        // index. Until then: a miss, never a false error.
        for item in items {
            stmt(item, Pos::Standalone, &mut out);
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

/// Where a statement sits. Every rule in [`REFUSED`] turns on this and nothing else.
///
/// The handler split is not arbitrary: `use_handlers` is threaded through both loaders, but
/// `Play._load_handlers` reaches `load_list_of_blocks` (`play.py:205`), which loads a top-level
/// entry as a Block without ever consulting the flag. Only `load_list_of_tasks` checks it
/// (`helpers.py:104-106`), one level down. A plain (non-block) entry *is* re-loaded through
/// that path, wrapped in an implicit block — which is why a block is legal as a handler and
/// fatal inside one, while a role include is fatal in both.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pos {
    /// A play's own `pre_tasks:`/`tasks:`/`post_tasks:`. Provably outside any role — measured
    /// still fatal for `meta: end_role` even when the play also has `roles:`.
    PlayTasks,
    /// A standalone task file. Could be a role's `tasks/main.yml`, its `handlers/main.yml`, or
    /// an include target — and the caller decides which, so the role- and handler-sensitive
    /// rules stay quiet here rather than guess. See the note on [`problems`].
    Standalone,
    /// A top-level entry of `handlers:`. A block here loads; row 1 starts below it.
    HandlerEntry,
    /// Inside a handler's `block:`/`rescue:`/`always:`.
    HandlerBody,
}

impl Pos {
    /// What the children of a node at this position are. Only the handler boundary moves.
    fn inside(self) -> Self {
        match self {
            Pos::PlayTasks => Pos::PlayTasks,
            Pos::Standalone => Pos::Standalone,
            Pos::HandlerEntry | Pos::HandlerBody => Pos::HandlerBody,
        }
    }
}

/// What a [`Refusal`] matches on. Three kinds because the node reads differ: a block is a
/// *shape*, a role include is a *module key*, and `meta:` needs the key **and** its value.
enum What {
    Block,
    /// Matched through [`keywords::core_action`], so the bare, `ansible.builtin.` and
    /// `ansible.legacy.` spellings all hit and a collection's own module never does.
    Action(&'static str),
    /// A `meta:` whose value is this word.
    Meta(&'static str),
}

enum Msg {
    Lit(&'static str),
    /// `{}` becomes the module key **as written**, FQCN included.
    Quoted(&'static str),
}

/// One row of the position table.
struct Refusal {
    what: What,
    at: &'static [Pos],
    msg: Msg,
    /// [`RULE_ID`] for the rows that replicate an ansible-core message verbatim; its own id
    /// for the one row whose message is ours.
    rule: &'static str,
}

const IN_HANDLERS: &[Pos] = &[Pos::HandlerEntry, Pos::HandlerBody];

/// Rows 1, 2, 6 and 25: everything ansible-core refuses purely because of *where* a statement
/// sits. One shape, so they are data rather than six near-identical predicates.
///
/// `Standalone` appears nowhere on purpose — see [`problems`].
const REFUSED: &[Refusal] = &[
    // Row 1 (`helpers.py:104-106`). A block written *as* a handler loads fine; only one nested
    // inside it is refused.
    Refusal {
        what: What::Block,
        at: &[Pos::HandlerBody],
        msg: Msg::Lit(BLOCK_AS_HANDLER),
        rule: RULE_ID,
    },
    // Row 2 (`helpers.py:245-247`), whose `%s` is the action as written — unlike rows 3-4,
    // whose messages are literal strings upstream, so the two cannot share a formatter.
    Refusal {
        what: What::Action("include_role"),
        at: IN_HANDLERS,
        msg: Msg::Quoted("Using '{}' as a handler is not supported."),
        rule: RULE_ID,
    },
    Refusal {
        what: What::Action("import_role"),
        at: IN_HANDLERS,
        msg: Msg::Quoted("Using '{}' as a handler is not supported."),
        rule: RULE_ID,
    },
    // Row 6a (`helpers.py:278-281`). `use_handlers` short-circuits before the role check, so
    // this fires in a role's own `handlers/` file too — which the ticket had backwards.
    Refusal {
        what: What::Meta("end_role"),
        at: IN_HANDLERS,
        msg: Msg::Lit(END_ROLE_HANDLER),
        rule: RULE_ID,
    },
    // Row 25 (`strategy/__init__.py:883`), the one raised at run time rather than at load.
    // Measured in both handler positions.
    Refusal {
        what: What::Meta("flush_handlers"),
        at: IN_HANDLERS,
        msg: Msg::Lit(FLUSH_AS_HANDLER),
        rule: RULE_ID,
    },
    // Row 6b (`helpers.py:283-285`), the `role is None` half. We never have to prove a
    // statement IS in a role — only to name the position where it provably is not.
    Refusal {
        what: What::Meta("end_role"),
        at: &[Pos::PlayTasks],
        msg: Msg::Lit(END_ROLE_OUTSIDE),
        rule: RULE_ID,
    },
    // `import_playbook:` in a task list — the one row here whose message is **ours**. Ansible
    // does fail, but only at run time (exit 2) and with a message about *parameters* that never
    // mentions position: a raw path gives `Action 'ansible.builtin.import_playbook' does not
    // support raw params.`, and a `{file: ...}` mapping gives `module (import_playbook) is
    // missing...`. Neither tells the author what is actually wrong, and there is no single one
    // to borrow — so this states the fault instead, on its own id.
    //
    // Every position, unlike its neighbours: the standalone-file miss the others take is about
    // role and handler ambiguity, and neither applies here. `Standalone` only ever sees this
    // nested inside a block, since an `import_playbook:` at the top level of a file makes
    // `keywords::is_play` call that file a playbook — so it routes to `play`, which peels the
    // entry off as the legitimate playbook-level statement it looks like.
    Refusal {
        what: What::Action("import_playbook"),
        at: &[
            Pos::PlayTasks,
            Pos::Standalone,
            Pos::HandlerEntry,
            Pos::HandlerBody,
        ],
        msg: Msg::Lit(IMPORT_PLAYBOOK_IN_TASKS),
        rule: MISPLACED_IMPORT_PLAYBOOK_RULE_ID,
    },
];

/// The last entry naming this action, with the spelling as written. Searched from the end for
/// the same reason [`Node::get`] is: on a duplicate key Ansible keeps the last.
fn action_entry<'a>(node: &'a Node, action: &str) -> Option<(Span, &'a str, &'a Node)> {
    node.entries().iter().rev().find_map(|(k, v)| {
        let key = k.as_str()?;
        (keywords::core_action(key) == action).then_some((k.span(), key, v))
    })
}

/// Walk the position table. Ansible raises on the first thing it refuses and stops loading, so
/// this reports one fault per statement and the caller does not recurse past it.
fn refused_here(node: &Node, pos: Pos, is_block: bool, out: &mut Vec<Problem>) -> bool {
    for rule in REFUSED {
        if !rule.at.contains(&pos) {
            continue;
        }
        let hit = match rule.what {
            What::Block => is_block.then(|| (node.span(), "")),
            What::Action(name) => action_entry(node, name).map(|(span, key, _)| (span, key)),
            What::Meta(word) => action_entry(node, "meta")
                .filter(|(_, _, value)| value.as_str() == Some(word))
                .map(|(span, key, _)| (span, key)),
        };
        let Some((span, written)) = hit else { continue };
        out.push(Problem {
            span,
            tier: Tier::Error,
            message: match rule.msg {
                Msg::Lit(m) => m.to_string(),
                Msg::Quoted(t) => t.replace("{}", written),
            },
            rule: rule.rule,
        });
        return true;
    }
    false
}

/// One entry of a task list: a block, whose three task-holding keys recurse, or a task.
fn stmt(node: &Node, pos: Pos, out: &mut Vec<Problem>) {
    match node {
        Node::Mapping { .. } => {}
        // A bare `-` is dropped before anything inspects it: `load_list_of_blocks` skips a
        // `None` entry (`helpers.py:53,65`), so it is not a fault — measured, loads clean.
        Node::Null { .. } => return,
        // An alias resolves to whatever the anchor holds, which may well be a mapping.
        // Judging it needs the substitution T-160 adds.
        Node::Other { .. } => return,
        // Everything else is fatal upstream, in every task list — measured in a play's
        // `tasks:`, inside a block, in `handlers:`, and in a role's `tasks/main.yml`.
        bad => {
            out.push(Problem {
                span: bad.span(),
                tier: Tier::Error,
                message: NOT_A_MAPPING.into(),
                rule: MALFORMED_TASK_ENTRY_RULE_ID,
            });
            return;
        }
    }
    // The same `Block.is_block` test `ast::build_stmt` makes, so the two agree on what a block
    // is. A `rescue:` with no `block:` is one — a malformed one, which is row 7.
    let is_block = keywords::BLOCK_TASK_CONTAINERS
        .iter()
        .any(|k| node.get(k).is_some());
    // Position rules first: every one of them raises before the statement is loaded at all, so
    // they beat the key-level rules below — measured on a handler include_role that also
    // carried a duplicate loop.
    if refused_here(node, pos, is_block, out) {
        return;
    }
    if is_block {
        rescue_without_block(node, out);
        for key in keywords::BLOCK_TASK_CONTAINERS {
            if let Some(list) = node.get(key) {
                for child in list.items() {
                    stmt(child, pos.inside(), out);
                }
            }
        }
        return;
    }
    // `ModuleArgsParser.parse()` runs in `load_list_of_tasks` before `Task.load`, so a mutual
    // exclusion it raises beats everything below — measured, on a task carrying both row 11's
    // fault and a duplicate loop. Row 24 is a warning of ours and suppresses nothing.
    if exclusions(node, On::Task, out) {
        return;
    }
    // Rows 12 and 22, the two ends of one candidate walk (`mod_args.py:330-368`). Row 11's
    // check is `mod_args.py:322`, above it, which is why the exclusions go first.
    if action_walk(node, out) {
        return;
    }
    // `preprocess_data` runs inside `Task.load`, so a duplicate loop — or a `with_*` with no
    // value — is raised before the field loaders run and long before `helpers.py` asks what
    // the action was. One fault, one message, in Ansible's own order.
    if duplicate_loop(node, out) {
        return;
    }
    // `_load_loop_control` is a field loader inside `Task.load`; the import rule is back up in
    // `load_list_of_tasks`, which only runs once `Task.load` has returned. So a fatal
    // `loop_control:` hides the import fault — measured, one message, not two.
    if loop_control_checks(node, out) {
        return;
    }
    loop_on_import(node, out);
}

/// Row 7. `_validate_rescue` and `_validate_always` are the same function
/// (`block.py:138-142`), and the guard is `if value and not self.block` — Python truthiness on
/// both sides, which is what makes the edges what they are:
///
/// - an **empty** `block: []` is falsy, so it counts as no block and the rule still fires;
/// - an empty `rescue: []` is falsy on the other side, so it is no fault at all;
/// - a null value for any of the three dies earlier in `_load` with its own message
///   (`A malformed block was encountered...`), so it is not this rule's to report.
///
/// `rescue` is reported before `always` whichever order they are written in — measured, and it
/// follows the FieldAttribute declaration order rather than the document.
fn rescue_without_block(node: &Node, out: &mut Vec<Problem>) {
    match node.get("block") {
        Some(Node::Null { .. }) => return,
        Some(block) if !block.items().is_empty() => return,
        _ => {}
    }
    for key in ["rescue", "always"] {
        let Some(value) = node.get(key) else { continue };
        if value.items().is_empty() {
            continue;
        }
        if let Some(span) = key_span(node, key) {
            out.push(error(span, format!("'{key}' keyword cannot be used without 'block'")));
        }
        // Ansible raises on the first of the two and stops, so one node gets one diagnostic.
        return;
    }
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
///
/// Returns whether row 21 fired, which suppresses the rules below it. T-155's warning does not:
/// Ansible loads that task without complaint, so anything it would go on to refuse is still a
/// fault the author has.
fn loop_control_checks(node: &Node, out: &mut Vec<Problem>) -> bool {
    let Some(lc) = node.get("loop_control") else { return false };
    let Some(anchor) = key_span(node, "loop_control") else { return false };
    if !matches!(lc, Node::Mapping { .. }) {
        out.push(error(anchor, LOOP_CONTROL_SHAPE.into()));
        return true;
    }
    if live_loop(node).is_some() {
        return false;
    }
    let keys: Vec<&str> = lc.entries().iter().filter_map(|(k, _)| k.as_str()).collect();
    if keys.is_empty() {
        return false;
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
    // Ours, and a warning: Ansible loads the task fine, so the rules after this one still apply.
    false
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
    // reaches any of these checks. It has one rule of its own on the way past.
    if node
        .entries()
        .iter()
        .any(|(k, _)| k.as_str().map(keywords::core_action) == Some("import_playbook"))
    {
        conflicting_import_playbook(node, out);
        return;
    }
    exclusions(node, On::Play, out);
    if let Some(hosts) = node.get("hosts") {
        self::hosts(hosts, src, out);
    }
    if let Some(prompts) = node.get("vars_prompt") {
        vars_prompt(prompts, out);
    }
    // `pre_tasks`, `tasks`, `post_tasks` and `handlers` all reach the same
    // `load_list_of_tasks`, so the task-shaped rules apply identically in each. Only row 1
    // cares which list it is, and only below the top level.
    for key in keywords::PLAY_TASK_CONTAINERS {
        let pos = if *key == "handlers" { Pos::HandlerEntry } else { Pos::PlayTasks };
        if let Some(list) = node.get(key) {
            for item in list.items() {
                stmt(item, pos, out);
            }
        }
    }
}

/// Which node kind an [`Exclusion`] applies to. The two are checked at different call sites,
/// so the table is filtered rather than the nodes being re-classified.
#[derive(Clone, Copy, PartialEq, Eq)]
enum On {
    Play,
    Task,
}

/// Rows 11, 13 and 24: two keys that cannot both sit on one node.
///
/// One shape, so they are data — but the `trigger_needs_value` flag is load-bearing and was
/// measured per row, not assumed. Row 13 is pure presence: `if 'user' in ds: if 'remote_user'
/// in ds` (`play.py:166-171`), so it fires even with both values null. Rows 11 and 24 are not:
/// their trigger key is read *before* the check, so a null value dies earlier in
/// `_normalize_parameters` with `unexpected parameter type in action: <class 'NoneType'>` — a
/// different message that is not ours to give.
struct Exclusion {
    on: On,
    /// The key that triggers the check, then the key it collides with.
    keys: (&'static str, &'static str),
    /// Whether the trigger key must carry a real value, per the note above.
    trigger_needs_value: bool,
    /// Which key the diagnostic underlines.
    anchor: &'static str,
    tier: Tier,
    rule: &'static str,
    msg: &'static str,
}

const EXCLUSIONS: &[Exclusion] = &[
    // Row 13 (`play.py:166-171`). Anchored on `user:`, the key the message says to drop.
    Exclusion {
        on: On::Play,
        keys: ("user", "remote_user"),
        trigger_needs_value: false,
        anchor: "user",
        tier: Tier::Error,
        rule: RULE_ID,
        msg: USER_AND_REMOTE_USER,
    },
    // Row 11 (`mod_args.py:322`). The raise lives in the `local_action` branch and fires when
    // the `action` branch above it already produced one, so `local_action` is the trigger the
    // author sees blamed — anchor there.
    Exclusion {
        on: On::Task,
        keys: ("local_action", "action"),
        trigger_needs_value: true,
        anchor: "local_action",
        tier: Tier::Error,
        rule: RULE_ID,
        msg: ACTION_AND_LOCAL_ACTION,
    },
    // Row 24 is **ours**: `local_action` sets `delegate_to = 'localhost'` (`mod_args.py:325`),
    // overwriting the value read at 303, and ansible-core says nothing at all. Proven with a
    // control: `delegate_to: other` alone runs `ok: [localhost -> other]`, and with a
    // `local_action` beside it the arrow disappears. WARNING, since the play does run — it
    // just runs somewhere the author did not ask for. Anchored on the key whose value is lost.
    Exclusion {
        on: On::Task,
        keys: ("local_action", "delegate_to"),
        trigger_needs_value: true,
        anchor: "delegate_to",
        tier: Tier::Warning,
        rule: DISCARDED_DELEGATE_TO_RULE_ID,
        msg: DISCARDED_DELEGATE_TO,
    },
];

/// Walk the exclusion table for one node. Unlike [`REFUSED`] every row is evaluated: a task can
/// carry row 11's fault and row 24's at once, and they are independent.
///
/// Returns whether a **fatal** one fired, which suppresses the rules downstream of it — row 24
/// is ours and a warning, so it never does.
fn exclusions(node: &Node, on: On, out: &mut Vec<Problem>) -> bool {
    let mut fatal = false;
    for ex in EXCLUSIONS {
        if ex.on != on {
            continue;
        }
        let Some(trigger) = node.get(ex.keys.0) else {
            continue;
        };
        if ex.trigger_needs_value && matches!(trigger, Node::Null { .. }) {
            continue;
        }
        if node.get(ex.keys.1).is_none() {
            continue;
        }
        if let Some(span) = key_span(node, ex.anchor) {
            out.push(Problem {
                span,
                tier: ex.tier,
                message: ex.msg.to_string(),
                rule: ex.rule,
            });
            fatal |= ex.tier == Tier::Error;
        }
    }
    fatal
}

/// The keys `ModuleArgsParser` treats as possible module names, in document order:
/// `non_task_ds` is everything that is not a Task or Handler attribute, not `local_action` or
/// `static`, and does not start with `with_` (`mod_args.py:128-132,330`).
///
/// [`keywords::is_task_directive`] is that set already, except for `static` — which is a task
/// attribute upstream but absent from our tables, so it is excluded by name here. A dotted key
/// is never a directive, matching `ast::find_action` and upstream both.
fn action_candidates(node: &Node) -> Vec<(Span, &str, &Node)> {
    node.entries()
        .iter()
        .filter_map(|(k, v)| {
            let key = k.as_str()?;
            let is_attr =
                key == "static" || (!key.contains('.') && keywords::is_task_directive(key));
            (!is_attr).then_some((k.span(), key, v))
        })
        .collect()
}

/// Row 12, and **no module resolution is involved** — the ticket had that wrong.
/// `load_list_of_tasks` calls `parse(skip_action_validation=True)` (`helpers.py:121`), and that
/// flag makes every surviving key an action candidate whether or not it names a real module.
/// Measured: `debug:` beside a `frobnicate:` conflicts, and `frobnicate` resolves to nothing.
/// The resolving parse is a *second* one inside `Task.load` (`task.py:305`), which only ever
/// sees a single candidate — which is why a lone unresolvable key gets `couldn't resolve
/// module/action` instead.
///
/// Anchored on the second key, the one whose arrival raises. Known miss: the first candidate's
/// value is normalized before the second is looked at, so a value `_normalize_parameters`
/// rejects raises `unexpected parameter type in action` there instead — measured on a list.
/// Scalars are assumed to be strings, since the parse tree keeps no scalar style; a bare int
/// first value is the one shape where we give the wrong message rather than none.
fn action_walk(node: &Node, out: &mut Vec<Problem>) -> bool {
    match action_candidates(node).as_slice() {
        // Row 22 (`mod_args.py:368`): nothing to run. `action:`/`local_action:` are Task
        // attributes so they never appear as candidates, but each sets the action in its own
        // branch above the walk — measured, both load clean alone.
        [] => {
            if node.get("action").is_some() || node.get("local_action").is_some() {
                return false;
            }
            out.push(error(node.span(), NO_MODULE.into()));
            true
        }
        [_] => false,
        // Row 12 (`mod_args.py:353-354`): the second candidate is the one that raises, so it is
        // the one underlined. The first's value is normalized before the second is looked at,
        // so a value `_normalize_parameters` rejects raises there instead — measured on a list.
        [first, second, ..] => {
            if matches!(first.2, Node::Sequence { .. }) {
                return false;
            }
            out.push(error(
                second.0,
                format!("conflicting action statements: {}, {}", first.1, second.1),
            ));
            true
        }
    }
}

/// Row 9. `PlaybookInclude.preprocess_data` collects the `import_playbook` spellings present as
/// a **set** and refuses unless exactly one survives (`playbook_include.py:41-48`):
///
/// ```python
/// keys = {action for action in C._ACTION_IMPORT_PLAYBOOK if action in ds}
/// if len(keys) != 1:
///     raise AnsibleError(f'Found conflicting import_playbook actions: {", ".join(sorted(keys))}')
/// ```
///
/// Two things fall out of that, both measured. The names are **sorted**, not written in
/// document order — `import_playbook:` written first still reports
/// `ansible.builtin.import_playbook, import_playbook`. And because it is a set, duplicates of
/// one spelling collapse: only *distinct* spellings count, which is why this is not an
/// [`EXCLUSIONS`] row — there is no fixed pair, any two of the three collide.
fn conflicting_import_playbook(node: &Node, out: &mut Vec<Problem>) {
    let mut spellings: Vec<&str> = node
        .entries()
        .iter()
        .filter_map(|(k, _)| k.as_str())
        .filter(|k| keywords::core_action(k) == "import_playbook")
        .collect();
    spellings.sort_unstable();
    spellings.dedup();
    if spellings.len() < 2 {
        return;
    }
    // Anchored on the last of them: the first is the one a reader takes as intended, so the
    // later spelling is the surprise.
    let anchor = node
        .entries()
        .iter()
        .rev()
        .find(|(k, _)| k.as_str().map(keywords::core_action) == Some("import_playbook"))
        .map(|(k, _)| k.span());
    if let Some(span) = anchor {
        out.push(error(
            span,
            format!(
                "Found conflicting import_playbook actions: {}",
                spellings.join(", ")
            ),
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
        // A non-empty scalar is the ordinary `hosts: web`, and also where the `hosts: 42` miss
        // above lands: no scalar style in the tree, so we take every scalar for a string.
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
        // An alias (`hosts: *webservers`). A **miss**, not a pass: ansible resolves the anchor
        // and validates what it holds — measured, `hosts: *bad` pointing at a mapping gives
        // this rule's own `must be a sequence or string`. Our parser turns every alias into
        // `Other` with no anchor table, so the value is invisible here. T-160.
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
        match item {
            Node::Mapping { .. } => {}
            // An alias resolves to whatever the anchor holds, which may well be a mapping. T-160.
            Node::Other { .. } => continue,
            // Every entry must be a mapping, and `preprocess_vars` says so before
            // `_load_vars_prompt` looks at a single key (`vars/manager.py:102-107`). A **null**
            // entry is fatal here — unlike in a task list, where `load_list_of_blocks` drops it.
            bad => {
                out.push(error(bad.span(), NOT_VARS_PROMPT_DATA.into()));
                continue;
            }
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

    /// Row 7, and the classifier fix underneath it. Before this, `Block.is_block`'s any-of-three
    /// was read as `block:` only, so a bare `rescue:` reached `find_action` and the keyword was
    /// taken for the module name — the node parsed as a well-formed task and we said nothing.
    #[test]
    fn rescue_or_always_without_a_block_is_an_error() {
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - rescue:\n        - debug: {msg: x}\n"),
            ["'rescue' keyword cannot be used without 'block'"]
        );
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - always:\n        - debug: {msg: x}\n"),
            ["'always' keyword cannot be used without 'block'"]
        );
        // A well-formed block stays silent however deeply it nests — measured legal at any
        // depth, and inside `rescue:`/`always:` lists too.
        assert!(check("- hosts: web\n  tasks:\n    - block:\n        - block:\n            - debug: {msg: x}\n").is_empty());
        assert!(check("- hosts: web\n  tasks:\n    - block:\n        - debug: {msg: b}\n      rescue:\n        - block:\n            - debug: {msg: r}\n").is_empty());
    }

    /// The guard is `if value and not self.block` — Python truthiness on both sides, so the
    /// edges are about emptiness, not about which keys are present.
    #[test]
    fn row_7_reads_emptiness_the_way_python_does() {
        // An empty `block: []` is falsy, so it counts as no block at all.
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - block: []\n      rescue:\n        - debug: {msg: r}\n"),
            ["'rescue' keyword cannot be used without 'block'"]
        );
        // An empty `rescue: []` is falsy on the other side, so there is nothing to complain of.
        assert!(check("- hosts: web\n  tasks:\n    - rescue: []\n").is_empty());
        assert!(check("- hosts: web\n  tasks:\n    - always: []\n").is_empty());
        assert!(check("- hosts: web\n  tasks:\n    - block: []\n").is_empty());
        // A null value dies earlier, in `_load`, with `A malformed block was encountered` — a
        // different message that is not this rule's to give.
        assert!(check("- hosts: web\n  tasks:\n    - rescue:\n").is_empty());
        assert!(check("- hosts: web\n  tasks:\n    - block:\n      rescue:\n        - debug: {msg: r}\n").is_empty());
    }

    /// `rescue` is validated before `always` whatever order they are written in — the
    /// FieldAttribute declaration order, not the document's. Ansible raises on the first and
    /// stops, so a node with both faults gets one diagnostic.
    #[test]
    fn row_7_reports_rescue_first_regardless_of_written_order() {
        for src in [
            "- hosts: web\n  tasks:\n    - rescue:\n        - debug: {msg: r}\n      always:\n        - debug: {msg: a}\n",
            "- hosts: web\n  tasks:\n    - always:\n        - debug: {msg: a}\n      rescue:\n        - debug: {msg: r}\n",
        ] {
            assert_eq!(check(src), ["'rescue' keyword cannot be used without 'block'"]);
        }
    }

    /// On a duplicate key Ansible keeps the last, and row 7 reads emptiness off that one.
    #[test]
    fn row_7_follows_the_last_of_a_duplicate_key() {
        // Last `block:` is empty, so the rescue has nothing to attach to.
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - block:\n        - debug: {msg: b}\n      block: []\n      rescue:\n        - debug: {msg: r}\n"),
            ["'rescue' keyword cannot be used without 'block'"]
        );
        // Last `block:` is the full one, so this is a well-formed block.
        assert!(check("- hosts: web\n  tasks:\n    - block: []\n      block:\n        - debug: {msg: b}\n      rescue:\n        - debug: {msg: r}\n").is_empty());
        // Last `always:` is empty, and an empty value is falsy.
        assert!(check("- hosts: web\n  tasks:\n    - block:\n        - debug: {msg: b}\n      always:\n        - debug: {msg: a}\n      always: []\n").is_empty());
    }

    /// The classifier fix also unblinds the walker: a rescue-only body was never recursed into,
    /// so every task-level rule was silent inside it.
    #[test]
    fn the_task_rules_reach_inside_a_malformed_block() {
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - rescue:\n        - import_tasks: f.yml\n          loop: [1]\n"),
            ["'rescue' keyword cannot be used without 'block'", NO_LOOP_TASKS]
        );
    }

    /// Row 1. `use_handlers` is only consulted by `load_list_of_tasks`, which a handler's own
    /// body reaches but the `handlers:` list itself does not — so the boundary is one level in,
    /// not the list. All three containers of a handler block reach it.
    #[test]
    fn a_block_nested_inside_a_handler_is_refused() {
        let handler = |body: &str| {
            format!("- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n{body}")
        };
        assert_eq!(
            check(&handler("      block:\n        - block:\n            - debug: {msg: x}\n")),
            [BLOCK_AS_HANDLER]
        );
        assert_eq!(
            check(&handler("      block:\n        - debug: {msg: x}\n      rescue:\n        - block:\n            - debug: {msg: r}\n")),
            [BLOCK_AS_HANDLER]
        );
        assert_eq!(
            check(&handler("      block:\n        - debug: {msg: x}\n      always:\n        - block:\n            - debug: {msg: a}\n")),
            [BLOCK_AS_HANDLER]
        );
    }

    /// The boundary itself, in both directions: a block written *as* a handler loads clean —
    /// `load_list_of_blocks` never consults the flag — and the same nesting outside `handlers:`
    /// is ordinary, legal Ansible.
    #[test]
    fn row_1_stops_at_the_handler_list_and_at_the_other_containers() {
        assert!(check(
            "- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      block:\n        - debug: {msg: x}\n"
        )
        .is_empty());
        for key in ["tasks", "pre_tasks", "post_tasks"] {
            assert!(
                check(&format!(
                    "- hosts: web\n  {key}:\n    - block:\n        - block:\n            - debug: {{msg: x}}\n"
                ))
                .is_empty(),
                "nested blocks are legal in {key}"
            );
        }
    }

    /// Ansible raises on the outermost nested block and stops loading, so a stack of them is one
    /// fault. Reporting one per level would turn a single mistake into a pile of squiggles.
    #[test]
    fn row_1_reports_a_stack_of_nested_blocks_once() {
        assert_eq!(
            check("- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      block:\n        - block:\n            - block:\n                - debug: {msg: x}\n"),
            [BLOCK_AS_HANDLER]
        );
    }

    /// Documented miss: a role's `handlers/main.yml` fires this upstream, but content alone
    /// cannot tell it from `tasks/main.yml`, where the same nesting is legal and common. A miss,
    /// never a false error — the same trade `hosts: 42` takes in batch 1.
    #[test]
    fn row_1_is_a_miss_in_a_standalone_file() {
        assert!(check("- name: h\n  block:\n    - block:\n        - debug: {msg: x}\n").is_empty());
    }

    /// Row 2. All six spellings `add_internal_fqcns` produces, each quoted back as written —
    /// measured one at a time on 2.21.2.
    #[test]
    fn a_role_include_in_a_handler_is_refused_in_every_core_spelling() {
        for action in [
            "include_role",
            "import_role",
            "ansible.builtin.include_role",
            "ansible.legacy.include_role",
            "ansible.builtin.import_role",
            "ansible.legacy.import_role",
        ] {
            assert_eq!(
                check(&format!(
                    "- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      {action}: {{name: r}}\n"
                )),
                [format!("Using '{action}' as a handler is not supported.")],
                "for {action}"
            );
        }
    }

    /// Unlike row 1, row 2 applies at a handler's top level as well as inside one: a plain entry
    /// there is wrapped into an implicit block and re-loaded through `load_list_of_tasks`, while
    /// a block entry is loaded directly and skips that check.
    #[test]
    fn row_2_applies_at_both_handler_depths() {
        assert_eq!(
            check("- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      block:\n        - include_role: {name: r}\n"),
            ["Using 'include_role' as a handler is not supported."]
        );
    }

    /// The boundaries: role includes are ordinary tasks outside `handlers:`, `include_tasks:` is
    /// fine as a handler, and a collection's own `include_role` is just a module — it is not in
    /// `_ACTION_ALL_PROPER_INCLUDE_IMPORT_ROLES`, and upstream fails it as an unresolvable
    /// action instead.
    #[test]
    fn row_2_leaves_everything_else_alone() {
        assert!(check("- hosts: web\n  tasks:\n    - include_role: {name: r}\n").is_empty());
        assert!(check("- hosts: web\n  tasks:\n    - import_role: {name: r}\n").is_empty());
        assert!(check(
            "- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      include_tasks: f.yml\n"
        )
        .is_empty());
        assert!(check(
            "- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      community.general.include_role: {name: r}\n"
        )
        .is_empty());
    }

    /// Row 2's raise sits before the task is loaded at all, so it wins over the loop rules that
    /// live in `preprocess_data` and in the import branch below it — measured both ways.
    #[test]
    fn row_2_beats_the_loop_rules_on_the_same_task() {
        assert_eq!(
            check("- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      include_role: {name: r}\n      loop: [1]\n      with_items: [2]\n"),
            ["Using 'include_role' as a handler is not supported."]
        );
        assert_eq!(
            check("- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      import_role: {name: r}\n      loop: [1]\n"),
            ["Using 'import_role' as a handler is not supported."]
        );
    }

    /// Rows 6a and 25, the two `meta:` refusals. Both fire at either handler depth, and both
    /// take every core spelling of `meta:` since the table matches through `core_action`.
    #[test]
    fn meta_end_role_and_flush_handlers_are_refused_as_handlers() {
        for (word, want) in [
            ("end_role", END_ROLE_HANDLER),
            ("flush_handlers", FLUSH_AS_HANDLER),
        ] {
            assert_eq!(
                check(&format!(
                    "- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      meta: {word}\n"
                )),
                [want],
                "top-level {word}"
            );
            assert_eq!(
                check(&format!(
                    "- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      block:\n        - meta: {word}\n"
                )),
                [want],
                "nested {word}"
            );
            assert_eq!(
                check(&format!(
                    "- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      ansible.builtin.meta: {word}\n"
                )),
                [want],
                "FQCN {word}"
            );
        }
    }

    /// Row 6b. We never prove a statement IS in a role — only that a play's own task list is a
    /// position where it provably is not. Measured still fatal alongside `roles:`, and inside a
    /// block, which is why `Pos::PlayTasks` survives `inside()`.
    #[test]
    fn meta_end_role_outside_a_role_is_an_error() {
        for key in ["pre_tasks", "tasks", "post_tasks"] {
            assert_eq!(
                check(&format!("- hosts: web\n  {key}:\n    - meta: end_role\n")),
                [END_ROLE_OUTSIDE],
                "in {key}"
            );
        }
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - block:\n        - meta: end_role\n"),
            [END_ROLE_OUTSIDE]
        );
        assert_eq!(
            check("- hosts: web\n  roles: [r]\n  tasks:\n    - meta: end_role\n"),
            [END_ROLE_OUTSIDE],
            "a play's own tasks are outside its roles"
        );
    }

    /// `meta:` words that are not refused anywhere, and the normal use of `flush_handlers` in a
    /// task list — the table must not turn every `meta:` into a diagnostic.
    #[test]
    fn other_meta_words_are_left_alone() {
        for word in ["clear_facts", "noop", "flush_handlers", "end_play"] {
            assert!(
                check(&format!("- hosts: web\n  tasks:\n    - meta: {word}\n")).is_empty(),
                "meta: {word} in tasks"
            );
        }
        for word in ["clear_facts", "noop"] {
            assert!(
                check(&format!(
                    "- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      meta: {word}\n"
                ))
                .is_empty(),
                "meta: {word} as a handler"
            );
        }
    }

    /// Every position rule is a documented miss in a standalone file: whether it is a handler
    /// file needs T-150, and whether it is inside a role needs T-020 — a byte-identical include
    /// target is legal from a role and fatal from a play, so the file alone has no answer.
    #[test]
    fn the_position_rules_are_all_misses_in_a_standalone_file() {
        for body in [
            "- name: h\n  block:\n    - block:\n        - debug: {msg: x}\n",
            "- name: h\n  include_role: {name: r}\n",
            "- name: h\n  meta: end_role\n",
            "- name: h\n  meta: flush_handlers\n",
        ] {
            assert!(check(body).is_empty(), "should stay quiet: {body:?}");
        }
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

    /// Row 11. The raise sits in `ModuleArgsParser`, which `load_list_of_tasks` calls *before*
    /// `Task.load` — so it beats the `preprocess_data` rules, measured on a task carrying both.
    #[test]
    fn action_and_local_action_are_mutually_exclusive() {
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - action: debug msg=x\n      local_action: debug msg=y\n"),
            [ACTION_AND_LOCAL_ACTION]
        );
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - action: debug msg=x\n      local_action: debug msg=y\n      loop: [1]\n      with_items: [2]\n"),
            [ACTION_AND_LOCAL_ACTION],
            "the exclusion beats the duplicate loop"
        );
        // Either alone is ordinary Ansible.
        assert!(check("- hosts: web\n  tasks:\n    - action: debug msg=x\n").is_empty());
        assert!(check("- hosts: web\n  tasks:\n    - local_action: debug msg=y\n").is_empty());
    }

    /// The `trigger_needs_value` flag, and why it is not cosmetic: a null `action:` dies in
    /// `_normalize_parameters` with `unexpected parameter type in action: <class 'NoneType'>`
    /// before the exclusion is reached, so claiming row 11 there would be the wrong message.
    /// Row 13 has no such guard — it is pure key presence and fires with both values null.
    #[test]
    fn a_null_trigger_belongs_to_a_different_rule() {
        assert!(
            check("- hosts: web\n  tasks:\n    - local_action:\n      action: debug msg=x\n").is_empty(),
            "null local_action is the parameter-type error, not row 11"
        );
        assert!(
            check("- hosts: web\n  tasks:\n    - local_action:\n      delegate_to: other\n").is_empty(),
            "and likewise for row 24"
        );
        // Row 13 is presence-only, measured fatal even with both values absent.
        assert_eq!(
            check("- hosts: web\n  user:\n  remote_user:\n  tasks: []\n"),
            [USER_AND_REMOTE_USER]
        );
    }

    /// Row 24, ours: `local_action` sets `delegate_to = 'localhost'` and overwrites what the
    /// author wrote. Proven with a control — `delegate_to: other` alone runs
    /// `ok: [localhost -> other]`, and the arrow disappears once `local_action` is beside it.
    /// A WARNING on its own id, since ansible-core accepts this and the play does run.
    #[test]
    fn a_discarded_delegate_to_is_our_own_warning() {
        let src = "- hosts: web\n  tasks:\n    - local_action: debug msg=y\n      delegate_to: other\n";
        let nodes = Document::new(src.to_string()).parse().unwrap();
        let got = problems(&nodes, src);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].tier, Tier::Warning);
        assert_eq!(got[0].rule, DISCARDED_DELEGATE_TO_RULE_ID);
        // The message has to carry the mechanism, or it reads as a style nit.
        for want in ["localhost", "discarded", "runs locally"] {
            assert!(got[0].message.contains(want), "missing {want:?}: {}", got[0].message);
        }
        // `delegate_to` with a plain module is the ordinary, correct spelling.
        assert!(check("- hosts: web\n  tasks:\n    - debug: {msg: y}\n      delegate_to: other\n").is_empty());
    }

    /// Row 12, and the headline is that no module resolution is involved: `load_list_of_tasks`
    /// passes `skip_action_validation=True`, so a key that names nothing at all is still an
    /// action candidate. Measured — `frobnicate` resolves to no module and still conflicts.
    #[test]
    fn two_action_candidates_conflict_without_resolving_either() {
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - debug: {msg: x}\n      frobnicate: y\n"),
            ["conflicting action statements: debug, frobnicate"]
        );
        // A typo'd keyword is an action candidate too, which is why upstream reports the
        // conflict rather than an unknown attribute — measured on `nmae` and `whne`.
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - debug: {msg: x}\n      nmae: foo\n"),
            ["conflicting action statements: debug, nmae"]
        );
        // FQCN keys are never directives, so they count and are quoted as written.
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - ansible.builtin.debug: {msg: x}\n      frobnicate: y\n"),
            ["conflicting action statements: ansible.builtin.debug, frobnicate"]
        );
    }

    /// What `non_task_ds` filters out (`mod_args.py:330`): task and handler attributes,
    /// `with_*`, `local_action`, and `static`. None of them is a second module.
    #[test]
    fn task_attributes_are_not_action_candidates() {
        for second in ["when: true", "with_items: [1]", "register: r", "listen: x"] {
            assert!(
                check(&format!(
                    "- hosts: web\n  tasks:\n    - debug: {{msg: x}}\n      {second}\n"
                ))
                .is_empty(),
                "{second} must not count as an action"
            );
        }
        // `static` is in `_task_attrs` upstream but absent from our tables, so it is excluded
        // by name — otherwise it would read as a second module. Its own diagnostic
        // (`'static' is not a valid attribute for a Task`) is T-107's, and matches upstream.
        assert!(check("- hosts: web\n  tasks:\n    - debug: {msg: x}\n      static: yes\n").is_empty());
    }

    /// The first candidate's value is normalized before the second is examined, so a value
    /// `_normalize_parameters` rejects raises there instead — measured on a list. A scalar is
    /// assumed to be a string, since the parse tree keeps no scalar style.
    #[test]
    fn row_12_defers_when_the_first_value_would_fail_normalization() {
        assert!(
            check("- hosts: web\n  tasks:\n    - foo: [1, 2]\n      bar: x\n").is_empty(),
            "a list first value is `unexpected parameter type in action` upstream"
        );
        for first in ["foo: \"a string\"", "foo:"] {
            assert_eq!(
                check(&format!("- hosts: web\n  tasks:\n    - {first}\n      bar: x\n")),
                ["conflicting action statements: foo, bar"],
                "for {first}"
            );
        }
    }

    /// Row 22, the far end of row 12's walk: no candidate at all. Measured on each of these.
    #[test]
    fn a_task_with_no_module_at_all() {
        for body in [
            "- name: just a name\n      when: true",
            "- name: just a name",
            "- {}",
            "- with_items: [1]",
        ] {
            assert_eq!(
                check(&format!("- hosts: web\n  tasks:\n    {body}\n")),
                [NO_MODULE],
                "for {body:?}"
            );
        }
    }

    /// `action:` and `local_action:` are Task attributes, so they never show up as candidates —
    /// but each sets the action in its own branch above the walk, so neither is row 22.
    /// Measured: both load clean on their own.
    #[test]
    fn action_and_local_action_each_supply_a_module() {
        assert!(check("- hosts: web\n  tasks:\n    - action: debug msg=x\n").is_empty());
        assert!(check("- hosts: web\n  tasks:\n    - local_action: debug msg=x\n").is_empty());
        // A block is not a task and never reaches this rule.
        assert!(check("- hosts: web\n  tasks:\n    - block:\n        - debug: {msg: x}\n").is_empty());
    }

    /// `import_playbook:` in a task list. Ours, not a replication: ansible fails at run time
    /// with a message about parameters that never mentions position, and which one you get
    /// depends on whether the value is a raw path or a mapping. Fires in every task position,
    /// since neither the role nor the handler ambiguity that limits its neighbours applies.
    #[test]
    fn import_playbook_in_a_task_list_is_our_own_rule() {
        let src = "- hosts: web\n  tasks:\n    - import_playbook: other.yml\n";
        let nodes = Document::new(src.to_string()).parse().unwrap();
        let got = problems(&nodes, src);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].rule, MISPLACED_IMPORT_PLAYBOOK_RULE_ID);
        assert_eq!(got[0].tier, Tier::Error);
        for want in ["top-level", "import_tasks"] {
            assert!(got[0].message.contains(want), "missing {want:?}: {}", got[0].message);
        }
        // Every task position, and a standalone task file too.
        for src in [
            "- hosts: web\n  pre_tasks:\n    - import_playbook: other.yml\n",
            "- hosts: web\n  tasks:\n    - block:\n        - import_playbook: other.yml\n",
            "- hosts: web\n  tasks: []\n  handlers:\n    - name: h\n      import_playbook: other.yml\n",
            // A standalone task file only ever reaches this nested: an `import_playbook:` at
            // the top level of one makes `is_play` call the whole file a playbook, so it goes
            // to `play` instead and is treated as the legitimate entry it looks like.
            "- block:\n    - import_playbook: other.yml\n",
        ] {
            assert_eq!(check(src).len(), 1, "for {src:?}");
        }
    }

    /// The whole point of the keyword, and it must stay silent: a playbook-level entry never
    /// reaches `stmt`, because `play` peels `import_playbook` entries off first.
    #[test]
    fn a_top_level_import_playbook_is_untouched() {
        assert!(check("- import_playbook: other.yml\n").is_empty());
        assert!(check("- import_playbook: a.yml\n- hosts: web\n  tasks: []\n").is_empty());
        assert!(check("- ansible.builtin.import_playbook: other.yml\n").is_empty());
    }

    /// A task-list entry that is not a mapping. Fatal upstream in every task list — measured in
    /// a play's `tasks:`, inside a block, in `handlers:`, and in a role's `tasks/main.yml` —
    /// and previously silent here, since `stmt` skipped non-mappings on the way in.
    #[test]
    fn a_task_list_entry_must_be_a_mapping() {
        for entry in ["just a string", "[1, 2]", "42"] {
            assert_eq!(
                check(&format!("- hosts: web\n  tasks:\n    - {entry}\n")),
                [NOT_A_MAPPING],
                "for {entry}"
            );
        }
        // Every position, since each one funnels through `stmt`.
        for src in [
            "- hosts: web\n  tasks:\n    - block:\n        - just a string\n",
            "- hosts: web\n  tasks: []\n  handlers:\n    - just a string\n",
            "- just a string\n",
        ] {
            assert_eq!(check(src), [NOT_A_MAPPING], "for {src:?}");
        }
    }

    /// Two shapes that are **not** faults, for opposite reasons. A bare `-` is dropped by
    /// `load_list_of_blocks` before anything inspects it — measured, loads clean. An alias
    /// resolves to whatever the anchor holds, so judging it waits on T-160.
    #[test]
    fn a_null_entry_and_an_alias_are_not_malformed() {
        assert!(check("- hosts: web\n  tasks:\n    -\n    - debug: {msg: x}\n").is_empty());
        assert!(check("- hosts: web\n  tasks:\n    - &anchor {debug: {msg: x}}\n    - *anchor\n").is_empty());
    }

    /// Row 9. A **set** of spellings, so any two of the three collide and the names come out
    /// sorted rather than in document order — both measured.
    #[test]
    fn conflicting_import_playbook_spellings() {
        assert_eq!(
            check("- import_playbook: a.yml\n  ansible.builtin.import_playbook: b.yml\n"),
            ["Found conflicting import_playbook actions: ansible.builtin.import_playbook, \
              import_playbook"],
            "sorted, not written order"
        );
        assert_eq!(
            check("- import_playbook: a.yml\n  ansible.builtin.import_playbook: b.yml\n  ansible.legacy.import_playbook: c.yml\n"),
            ["Found conflicting import_playbook actions: ansible.builtin.import_playbook, \
              ansible.legacy.import_playbook, import_playbook"]
        );
        // One spelling is the whole point of the keyword.
        assert!(check("- import_playbook: a.yml\n").is_empty());
        assert!(check("- ansible.builtin.import_playbook: a.yml\n").is_empty());
        // A collection's own module is not an import_playbook at all.
        assert!(check("- import_playbook: a.yml\n  community.general.import_playbook: b.yml\n").is_empty());
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

    /// A fatal `loop_control:` is raised inside `Task.load`, so `load_list_of_tasks` never gets
    /// to ask whether the action was an import — measured, one message, not two.
    #[test]
    fn a_fatal_loop_control_suppresses_the_import_loop_rule() {
        let src = "- hosts: web\n  tasks:\n    - import_tasks: t.yml\n      loop: [1, 2]\n      \
                   loop_control: 5\n";
        assert_eq!(
            check(src),
            ["the `loop_control` value must be specified as a dictionary and cannot be a \
              variable itself (though it can contain variables)"]
        );
    }

    /// Row 27. Every shape that is not a mapping, measured fatal on 2.21.2.
    #[test]
    fn a_vars_prompt_entry_that_is_not_a_mapping_is_refused() {
        let cases = [
            "- hosts: web\n  vars_prompt:\n    - just a string\n  tasks: []\n",
            "- hosts: web\n  vars_prompt:\n    - 42\n  tasks: []\n",
            // Fatal here, where the same entry in a task list is dropped and loads clean.
            "- hosts: web\n  vars_prompt:\n    -\n  tasks: []\n",
            "- hosts: web\n  vars_prompt:\n    - - name: pw\n  tasks: []\n",
            // Not a list at all: `preprocess_vars` wraps it, then refuses the wrapped entry.
            "- hosts: web\n  vars_prompt: just a string\n  tasks: []\n",
        ];
        for src in cases {
            assert_eq!(check(src), ["Invalid variable file contents."], "{src}");
        }
        // The anchor may hold a mapping — measured, `- *entry` loads clean. Silent until T-160.
        let aliased = "- hosts: web\n  vars:\n    e: &e {name: pw}\n  vars_prompt:\n    - *e\n  \
                       tasks: []\n";
        assert!(check(aliased).is_empty(), "{:?}", check(aliased));
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
