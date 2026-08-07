//! Ansible keyword schema — which keys are *directives* (Ansible-owned) versus the
//! *module* on a task, and which keys hold nested task lists.
//!
//! Derived from the `FieldAttribute` declarations in Ansible's own source
//! (`lib/ansible/playbook/{base,play,task,block,conditional,taggable,collectionsearch,
//! delegatable,notifiable}.py`). This is a checked-in snapshot so the LSP works with no
//! Ansible clone present. To regenerate against a newer Ansible, grep those files for
//! `= FieldAttribute` / `= NonInheritableFieldAttribute` and fold the names in below.
//!
//! The lists err toward *completeness*: a directive we forget would be misread as a task's
//! module, whereas an extra directive name is harmless (no module is named `when`).

/// Shared by Play, Task and Block (`base.py`) plus the universally-mixed traits
/// (`when`/`tags`/`collections`/`delegate_*`). Over-including here is safe.
const COMMON: &[&str] = &[
    "any_errors_fatal",
    "become",
    "become_exe",
    "become_flags",
    "become_method",
    "become_user",
    "check_mode",
    "collections",
    "connection",
    "debugger",
    "delegate_facts",
    "delegate_to",
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
    "tags",
    "throttle",
    "timeout",
    "vars",
    "when",
];

/// `task.py` + the task-only mixins, plus the parser-level keys that aren't
/// `FieldAttribute`s but still aren't the module: `action`/`local_action`/`args`, and
/// `listen` on handlers. `with_*` is matched by prefix, not listed.
const TASK_ONLY: &[&str] = &[
    "action",
    "args",
    "async",
    "changed_when",
    "delay",
    "failed_when",
    "listen",
    "local_action",
    "loop",
    "loop_control",
    "notify",
    "poll",
    "register",
    "retries",
    "until",
];

/// `play.py`. Includes the task-container keys (`pre_tasks`/`tasks`/`post_tasks`/
/// `handlers`) and `roles`.
const PLAY_ONLY: &[&str] = &[
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

/// `block.py`.
const BLOCK_ONLY: &[&str] = &["always", "block", "rescue"];

/// Keys on an `include_role`/`import_role` (and `roles:` dict entries) that name where in
/// the role to look — the siblings of `tasks_from`. Not `FieldAttribute`s; from role
/// include preprocessing.
pub const ROLE_INCLUDE_KEYS: &[&str] = &[
    "allow_duplicates",
    "defaults_from",
    "handlers_from",
    "name",
    "public",
    "role",
    "rolespec_validate",
    "tasks_from",
    "vars_from",
];

/// Play-level keys whose value is an ordered list of tasks/blocks.
pub const PLAY_TASK_CONTAINERS: &[&str] = &["pre_tasks", "tasks", "post_tasks", "handlers"];

/// Block-level keys whose value is an ordered list of tasks/blocks.
pub const BLOCK_TASK_CONTAINERS: &[&str] = &["block", "rescue", "always"];

fn in_set(sets: &[&[&str]], key: &str) -> bool {
    sets.iter().any(|s| s.contains(&key))
}

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
/// count as directives.
pub fn is_task_directive(key: &str) -> bool {
    key.starts_with("with_") || in_set(&[COMMON, TASK_ONLY], key)
}

/// Is `key` a directive on a play?
pub fn is_play_directive(key: &str) -> bool {
    in_set(&[COMMON, PLAY_ONLY], key)
}

/// Is `key` a directive on a block?
pub fn is_block_directive(key: &str) -> bool {
    in_set(&[COMMON, BLOCK_ONLY], key)
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
        assert!(is_task_directive("async")); // the YAML key, not async_val
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
}
