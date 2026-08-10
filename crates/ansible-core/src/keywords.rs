//! Ansible keyword schema — which keys are *directives* (Ansible-owned) versus the
//! *module* on a task, and which keys are legal in each playbook context.
//!
//! Derived from the `Attribute` declarations in Ansible's own source, gathered the way
//! `FieldAttributeBase._fattributes` gathers them (`base.py:93-105`): every `Attribute`
//! in the class MRO, under its name *and* its alias — which is why `async`, `async_val`
//! and `loop_with` are all legal on a task. This is a checked-in snapshot so the LSP works
//! with no Ansible clone present. To regenerate against a newer Ansible, run the
//! fattributes oracle from the source checkout and diff (T-107):
//!
//! ```text
//! python3 -c "import sys; sys.path.insert(0,'lib'); \
//!   from ansible.playbook.play import Play; print(sorted(Play.fattributes))"
//! ```
//!
//! Two audiences with opposite error costs share this file:
//!
//! - **Module detection** (`is_task_directive` and friends) errs toward *over*-inclusion:
//!   a directive we forget would be misread as a task's module, whereas an extra directive
//!   name is harmless (no module is named `when`).
//! - **Validation** ([`legal_key`]) is *exact* per context: these sets reproduce
//!   `'%s' is not a valid attribute for a %s` (`base.py:211-220`), so both a missing and
//!   an extra name is a wrong diagnostic.

// ---------------------------------------------------------------------------------------
// The mixins, exactly as ansible-core composes them.
// ---------------------------------------------------------------------------------------

/// `Base` (`base.py:688-721`): inherited by every playbook object.
const BASE: &[&str] = &[
    "any_errors_fatal",
    "become",
    "become_exe",
    "become_flags",
    "become_method",
    "become_user",
    "check_mode",
    "connection",
    "debugger",
    "diff",
    "environment",
    "ignore_errors",
    "ignore_unreachable",
    "module_defaults",
    "name",
    "no_log",
    "port",
    "remote_user",
    "run_once",
    "throttle",
    "timeout",
    "vars",
];

/// `Conditional` (`conditional.py:32`).
const CONDITIONAL: &[&str] = &["when"];

/// `Taggable` (`taggable.py:46`).
const TAGGABLE: &[&str] = &["tags"];

/// `CollectionSearch` (`collectionsearch.py:34`).
const COLLECTION_SEARCH: &[&str] = &["collections"];

/// `Delegatable` (`delegatable.py:10-11`).
const DELEGATABLE: &[&str] = &["delegate_to", "delegate_facts"];

/// `Notifiable` (`notifiable.py:10`).
const NOTIFIABLE: &[&str] = &["notify"];

// ---------------------------------------------------------------------------------------
// Class-own keys.
// ---------------------------------------------------------------------------------------

/// `Play` (`play.py:61-89`). Play mixes in **only** `Taggable` and `CollectionSearch`
/// (`play.py:48`) — `when:`, `notify:` and `delegate_to:` on a play are fatal.
const PLAY_OWN: &[&str] = &[
    "fact_path",
    "force_handlers",
    "gather_facts",
    "gather_subset",
    "gather_timeout",
    "handlers",
    "hosts",
    "max_fail_percentage",
    "order",
    "post_tasks",
    "pre_tasks",
    "roles",
    "serial",
    "strategy",
    "tasks",
    "validate_argspec",
    "vars_files",
    "vars_prompt",
];

/// `Block` (`block.py:35-37`); mixes in all five traits (`block.py:32`).
const BLOCK_OWN: &[&str] = &["always", "block", "rescue"];

/// `Task` (`task.py:79-94`); same five traits (`task.py:54`). Includes every raw
/// `fattributes` key: `async` is the YAML alias of `async_val` and *both* are accepted,
/// as is the nominally-private `loop_with`. `action`/`args` are real attributes here;
/// `local_action` is not — it is parser-level (`mod_args.py:131`).
const TASK_OWN: &[&str] = &[
    "action",
    "args",
    "async",
    "async_val",
    "changed_when",
    "delay",
    "failed_when",
    "loop",
    "loop_control",
    "loop_with",
    "poll",
    "register",
    "retries",
    "until",
];

/// `Handler` = Task + `listen` (`handler.py:27`).
const HANDLER_OWN: &[&str] = &["listen"];

/// `RoleMetadata` (`metadata.py:38-41`); mixes in only `CollectionSearch`
/// (`metadata.py:32`) — `when:` in `meta/main.yml` is fatal, `become:` parses fine.
const ROLE_METADATA_OWN: &[&str] = &[
    "allow_duplicates",
    "argument_specs",
    "dependencies",
    "galaxy_info",
];

/// Task-level keys on a **dynamic** include (`include_tasks`/`include_role`):
/// `TaskInclude.VALID_INCLUDE_KEYWORDS` (`task_include.py:42-44`). A smaller set than
/// Task — `become:`, `delegate_to:`, `until:` on an `include_tasks` are invalid.
/// Handler context adds `listen` (`handler_task_include.py:27`). `import_tasks`/
/// `import_role` skip this restriction and keep the full Task set.
const DYNAMIC_INCLUDE: &[&str] = &[
    "action",
    "args",
    "collections",
    "debugger",
    "ignore_errors",
    "loop",
    "loop_control",
    "loop_with",
    "name",
    "no_log",
    "register",
    "run_once",
    "tags",
    "timeout",
    "vars",
    "when",
];

/// `loop_control:` values load as `LoopControl` (`loop_control.py:27-35`), which extends
/// `FieldAttributeBase` directly — none of the common keywords are legal under it.
pub const LOOP_CONTROL_KEYS: &[&str] = &[
    "break_when",
    "extended",
    "extended_allitems",
    "index_var",
    "label",
    "loop_var",
    "pause",
];

/// Args of `include_tasks`/`import_tasks`: `TaskInclude.VALID_ARGS` minus the internal
/// `_raw_params` (the free-form spelling, which is not a written key)
/// (`task_include.py:39-41`). A closed set — anything else is a hard `Invalid options`
/// error (`task_include.py:70-72`), and `apply` is additionally fatal on `import_tasks`
/// (`task_include.py:79-81`).
pub const TASK_INCLUDE_ARGS: &[&str] = &["apply", "file"];

/// Args of `include_role`/`import_role`: `IncludeRole.VALID_ARGS`
/// (`role_include.py:40-43`). A closed set — an unknown arg is a hard
/// `Invalid options` error (`role_include.py:137-139`). `apply` and `rescuable` are
/// additionally fatal on `import_role` (`role_include.py:150-159`).
pub const ROLE_INCLUDE_KEYS: &[&str] = &[
    "allow_duplicates",
    "apply",
    "defaults_from",
    "handlers_from",
    "name",
    "public",
    "rescuable",
    "role",
    "rolespec_validate",
    "tasks_from",
    "vars_from",
];

/// Tag names ansible reserves for `--tags`/`--skip-tags` selection (`taggable.py:42`). Using
/// one as a real tag is a warning, not an error — it loads and then behaves unexpectedly.
pub const RESERVED_TAGS: &[&str] = &["all", "tagged", "untagged"];

/// Keys of one `vars_prompt:` entry (`play.py:246`). A closed set checked inline at load,
/// not a `FieldAttribute` class — an unknown key is `Invalid vars_prompt data structure,
/// found unsupported key '%s'`, fatal regardless of `invalid_task_attribute_failed`.
pub const VARS_PROMPT_KEYS: &[&str] = &[
    "confirm",
    "default",
    "encrypt",
    "name",
    "private",
    "prompt",
    "salt",
    "salt_size",
    "unsafe",
];

/// Play-level keys whose value is an ordered list of tasks/blocks.
pub const PLAY_TASK_CONTAINERS: &[&str] = &["pre_tasks", "tasks", "post_tasks", "handlers"];

/// Block-level keys whose value is an ordered list of tasks/blocks.
pub const BLOCK_TASK_CONTAINERS: &[&str] = &["block", "rescue", "always"];

fn in_set(sets: &[&[&str]], key: &str) -> bool {
    sets.iter().any(|s| s.contains(&key))
}

// ---------------------------------------------------------------------------------------
// Exact validation sets, per context (T-107).
// ---------------------------------------------------------------------------------------

/// A place a YAML key can appear, each with its own legal keyword set. Mirrors the class
/// that would load the mapping in ansible-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyContext {
    Play,
    Block,
    Task,
    Handler,
    /// A task whose action is `include_tasks`/`include_role` (`constants.py:45`) — the
    /// restricted `VALID_INCLUDE_KEYWORDS` set, not the full Task set.
    DynamicInclude,
    /// A dynamic include in handler position: the restricted set plus `listen`.
    DynamicHandlerInclude,
    /// `meta/main.yml`.
    RoleMetadata,
    /// The mapping under `loop_control:`.
    LoopControl,
}

/// The sets whose union is the legal vocabulary of `ctx`, plus preprocess-level escapes
/// that never reach `fattributes` (`user:` on a play, `play.py:166-174`).
fn sets_of(ctx: KeyContext) -> (&'static [&'static [&'static str]], &'static [&'static str]) {
    const TASK_SETS: &[&[&str]] = &[
        BASE,
        CONDITIONAL,
        TAGGABLE,
        COLLECTION_SEARCH,
        DELEGATABLE,
        NOTIFIABLE,
        TASK_OWN,
    ];
    match ctx {
        KeyContext::Play => (&[BASE, TAGGABLE, COLLECTION_SEARCH, PLAY_OWN], &["user"]),
        KeyContext::Block => (
            &[BASE, CONDITIONAL, TAGGABLE, COLLECTION_SEARCH, DELEGATABLE, NOTIFIABLE, BLOCK_OWN],
            &[],
        ),
        KeyContext::Task => (TASK_SETS, &[]),
        KeyContext::Handler => (
            &[BASE, CONDITIONAL, TAGGABLE, COLLECTION_SEARCH, DELEGATABLE, NOTIFIABLE, TASK_OWN,
              HANDLER_OWN],
            &[],
        ),
        KeyContext::DynamicInclude => (&[DYNAMIC_INCLUDE], &[]),
        KeyContext::DynamicHandlerInclude => (&[DYNAMIC_INCLUDE, HANDLER_OWN], &[]),
        KeyContext::RoleMetadata => (&[BASE, COLLECTION_SEARCH, ROLE_METADATA_OWN], &[]),
        KeyContext::LoopControl => (&[LOOP_CONTROL_KEYS], &[]),
    }
}

/// Is `key` accepted by ansible-core in this context? Exact — reproduces the
/// `frozenset(self.fattributes)` membership check of `base.py:217-220`, plus the
/// preprocess-level escapes that run before it (`user:` on a play, `play.py:166-174`).
///
/// The module key on a task and `local_action`/`with_*` are *not* in these sets — they
/// are consumed by the args parser before validation, so callers must exclude them first
/// (`mod_args.py:330-333`), as must role params on a `roles:` entry
/// (`definition.py:200-224`).
pub fn legal_key(ctx: KeyContext, key: &str) -> bool {
    let (sets, escapes) = sets_of(ctx);
    escapes.contains(&key) || in_set(sets, key)
}

/// Every key [`legal_key`] accepts in `ctx`, for near-miss suggestions. Unsorted and
/// possibly with duplicates across mixins — callers scan, they don't display the list.
pub fn legal_keys(ctx: KeyContext) -> impl Iterator<Item = &'static str> {
    let (sets, escapes) = sets_of(ctx);
    sets.iter().flat_map(|s| s.iter()).chain(escapes.iter()).copied()
}

// ---------------------------------------------------------------------------------------
// Lenient directive predicates, for module detection.
// ---------------------------------------------------------------------------------------

/// The action name of a task key, for exactly the spellings Ansible recognises as core:
/// bare, `ansible.builtin.`- or `ansible.legacy.`-prefixed (`constants.py:34`,
/// `utils/fqcn.py:20-31`). Any other dotted key comes back whole, so it can never equal a
/// bare action name — `community.general.include_tasks` is an ordinary module in that
/// collection, not an include (T-094).
pub fn core_action(key: &str) -> &str {
    key.strip_prefix("ansible.builtin.")
        .or_else(|| key.strip_prefix("ansible.legacy."))
        .unwrap_or(key)
}

/// Is `key` an Ansible directive on a task (as opposed to the module)? `key` is the bare
/// key — directives are never FQCN, so a dotted key is always the module. `with_*` loops
/// count as directives (Ansible only accepts `with_<lookup>` for an installed lookup,
/// `task.py:336`; we accept the prefix wholesale, erring lenient since we cannot
/// enumerate collection lookups). `local_action` and `listen` are included: over-inclusion
/// is safe here.
pub fn is_task_directive(key: &str) -> bool {
    key.starts_with("with_")
        || key == "local_action"
        || legal_key(KeyContext::Handler, key)
}

/// Is `key` a directive on a play? Lenient (accepts the whole task vocabulary too):
/// used to split directives from modules, not to validate — `when:` on a play is fatal
/// to Ansible, but it is still a directive, not a module.
pub fn is_play_directive(key: &str) -> bool {
    legal_key(KeyContext::Play, key) || is_task_directive(key)
}

/// Is `key` a directive on a block? Lenient, same reasoning.
pub fn is_block_directive(key: &str) -> bool {
    legal_key(KeyContext::Block, key) || in_set(&[BLOCK_OWN], key)
}

/// Does this mapping look like a play? Plays are the only nodes with `hosts:`, and a
/// bare `import_playbook:` entry sits in the same top-level sequence.
pub fn is_play(keys: impl Iterator<Item = impl AsRef<str>>) -> bool {
    keys.into_iter().any(|k| {
        let s = k.as_ref();
        // `import_playbook` may be written `ansible.builtin.`/`ansible.legacy.`-prefixed.
        s == "hosts" || core_action(s) == "import_playbook"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_keys_are_not_directives() {
        // The module on a task is whatever is left after directives are removed.
        assert!(!is_task_directive("debug"));
        assert!(!is_task_directive("command"));
        assert!(!is_task_directive("include_tasks")); // an action, not a directive
    }

    #[test]
    fn known_directives_classify() {
        for k in ["when", "loop", "register", "become", "tags", "vars", "notify"] {
            assert!(is_task_directive(k), "{k} should be a task directive");
        }
        assert!(is_task_directive("with_items"));
        assert!(is_task_directive("async")); // the YAML key, alias of async_val
        assert!(is_task_directive("local_action"));
    }

    #[test]
    fn play_vs_task_scope() {
        assert!(is_play_directive("hosts"));
        assert!(!is_task_directive("hosts")); // hosts is a play key, not a task's module
        assert!(is_block_directive("block"));
        assert!(!is_play_directive("block"));
    }

    #[test]
    fn play_detection() {
        assert!(is_play(["hosts", "tasks"].iter()));
        assert!(is_play(["import_playbook"].iter()));
        assert!(!is_play(["include_tasks", "when"].iter()));
    }

    /// The context sets match `cls.fattributes` exactly. The counts are from the oracle
    /// run against ansible-core (raw dict keys, names and aliases both); if a count here
    /// drifts on a version bump, re-run the oracle and update the sets, not the test.
    #[test]
    fn set_sizes_match_the_fattributes_oracle() {
        let count = |ctx, extra: &[&str]| {
            let mut all: Vec<&str> = [
                BASE, CONDITIONAL, TAGGABLE, COLLECTION_SEARCH, DELEGATABLE, NOTIFIABLE,
                PLAY_OWN, BLOCK_OWN, TASK_OWN, HANDLER_OWN, ROLE_METADATA_OWN,
                DYNAMIC_INCLUDE, LOOP_CONTROL_KEYS,
            ]
            .concat();
            all.extend(extra);
            all.sort_unstable();
            all.dedup();
            all.into_iter().filter(|k| legal_key(ctx, k)).count()
        };
        assert_eq!(count(KeyContext::Play, &["user"]), 43); // oracle: 42, + legacy `user`
        assert_eq!(count(KeyContext::Block, &[]), 31);
        assert_eq!(count(KeyContext::Task, &[]), 42);
        assert_eq!(count(KeyContext::Handler, &[]), 43);
        assert_eq!(count(KeyContext::RoleMetadata, &[]), 27);
        assert_eq!(count(KeyContext::DynamicInclude, &[]), 16);
        assert_eq!(count(KeyContext::DynamicHandlerInclude, &[]), 17);
        assert_eq!(count(KeyContext::LoopControl, &[]), 7);
    }

    #[test]
    fn the_negative_space_is_where_the_diagnostics_live() {
        // Play mixes in no Conditional/Delegatable/Notifiable (`play.py:48`).
        for k in ["when", "delegate_to", "delegate_facts", "notify", "loop", "register"] {
            assert!(!legal_key(KeyContext::Play, k), "{k} must be fatal on a play");
        }
        // ...but the legacy `user:` escape hatch is real (`play.py:166-174`).
        assert!(legal_key(KeyContext::Play, "user"));
        assert!(!legal_key(KeyContext::Task, "user"));

        // Blocks look like tasks but have no loop/register/action.
        for k in ["loop", "register", "action", "until", "listen"] {
            assert!(!legal_key(KeyContext::Block, k), "{k} must be fatal on a block");
        }
        assert!(legal_key(KeyContext::Block, "block"));

        // `listen` is handler-only.
        assert!(legal_key(KeyContext::Handler, "listen"));
        assert!(!legal_key(KeyContext::Task, "listen"));

        // Dynamic includes lose most of the task vocabulary (`task_include.py:42-44`).
        for k in ["become", "delegate_to", "until", "changed_when", "retries", "async"] {
            assert!(
                !legal_key(KeyContext::DynamicInclude, k),
                "{k} must be invalid on include_tasks/include_role"
            );
        }
        assert!(legal_key(KeyContext::DynamicInclude, "when"));
        assert!(legal_key(KeyContext::DynamicHandlerInclude, "listen"));

        // meta/main.yml: no Conditional, but all of Base parses.
        assert!(!legal_key(KeyContext::RoleMetadata, "when"));
        assert!(legal_key(KeyContext::RoleMetadata, "become"));
        assert!(legal_key(KeyContext::RoleMetadata, "galaxy_info"));

        // loop_control skips Base entirely (`loop_control.py:27`).
        assert!(!legal_key(KeyContext::LoopControl, "name"));
        assert!(legal_key(KeyContext::LoopControl, "loop_var"));
    }

    #[test]
    fn raw_fattributes_quirks_are_legal() {
        // `fattributes` holds names AND aliases (`base.py:102-104`), so all three are
        // accepted YAML on a task — verified live via `Task.load`.
        for k in ["async", "async_val", "loop_with"] {
            assert!(legal_key(KeyContext::Task, k), "{k} is accepted by Ansible");
        }
    }
}
