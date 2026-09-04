//! The names ansible sets without any workspace file defining them, each with the scope it
//! is set in (T-224).
//!
//! The table was diffed against a run, not typed from the docs. Recipe, ansible-core 2.21.2,
//! `gather_facts: false`:
//!
//! ```yaml
//! - debug: msg="{{ query('varnames', '.*') | sort | join(' ') }}"
//! ```
//!
//! at a play task, a looped task (with and without `loop_control: extended` / `index_var` /
//! `loop_var`), a task inside a role, inside a role included from another role, and a
//! delegated task. Every name below appeared in exactly the scope it carries, and a name
//! read outside its scope was measured fatal (`'item' is undefined`, `'role_name' is
//! undefined`). `omit` is the one name `varnames` does not list — it is a templar special —
//! and `omit is defined` answered `True`.
//!
//! Not in the table on purpose: `_task` (internal), and the `ansible_<fact>` set, which only
//! exists after `setup` runs and is host-dependent — see [`may_be_fact`].

/// Where a name is set, and so where a read of it is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every task of every play.
    Always,
    /// A task with `loop:` or `with_*`. Also the task's own `when:`, `vars:` and
    /// `loop_control: label`, which are evaluated per item — but not the `loop:` value
    /// itself, which is evaluated once, before there is an item (measured fatal).
    Loop,
    /// A looped task with `loop_control: extended: true`.
    ExtendedLoop,
    /// A looped task with `loop_control: index_var:` set.
    IndexVar,
    /// A task inside a role — its tasks, handlers, templates, and the role's own vars.
    Role,
    /// A role that was itself included or imported from another role.
    ChildRole,
    /// A task with `delegate_to:`.
    Delegated,
}

impl Scope {
    /// One clause for a hover: where the name is present.
    pub fn describe(self) -> &'static str {
        match self {
            Scope::Always => "present in every task",
            Scope::Loop => "present only inside a `loop:` / `with_*` task",
            Scope::ExtendedLoop => "present only in a loop with `loop_control: extended: true`",
            Scope::IndexVar => "present only in a loop with `loop_control: index_var:` set",
            Scope::Role => "present only inside a role",
            Scope::ChildRole => "present only in a role included or imported from another role",
            Scope::Delegated => "present only on a task with `delegate_to:`",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Injected {
    pub name: &'static str,
    pub scope: Scope,
    /// The ansible-core layer that sets it — what a reader would grep for.
    pub set_by: &'static str,
    /// One line: what the value is.
    pub meaning: &'static str,
}

const MAGIC: &str = "VariableManager._get_magic_variables";
const OPTIONS: &str = "the CLI options (`VariableManager._options_vars`)";
const PLAY_CONTEXT: &str = "PlayContext.update_vars, at task execution";
const GET_VARS: &str = "VariableManager.get_vars";
const LOOP: &str = "TaskExecutor, per loop item";
const ROLE: &str = "Role.get_role_params / RoleInclude";

macro_rules! row {
    ($name:literal, $scope:ident, $set_by:expr, $meaning:literal) => {
        Injected { name: $name, scope: Scope::$scope, set_by: $set_by, meaning: $meaning }
    };
}

pub const TABLE: &[Injected] = &[
    // --- unprefixed, every task ---
    row!("inventory_hostname", Always, MAGIC, "the current host's name as the inventory spells it"),
    row!("inventory_hostname_short", Always, MAGIC, "`inventory_hostname` up to the first dot"),
    row!("groups", Always, MAGIC, "dict of every group to its list of host names"),
    row!("group_names", Always, MAGIC, "list of the groups the current host is in"),
    row!("hostvars", Always, GET_VARS, "dict of every host to its variables, as seen with no play and no task"),
    row!("inventory_dir", Always, OPTIONS, "directory of the inventory source that first defined the current host"),
    row!("inventory_file", Always, OPTIONS, "path of the inventory source that first defined the current host"),
    row!("playbook_dir", Always, MAGIC, "directory of the playbook `ansible-playbook` was given"),
    row!("play_hosts", Always, MAGIC, "deprecated alias of `ansible_play_batch`, removed in 2.23 — the hosts in the current batch"),
    row!("role_names", Always, MAGIC, "names of the roles in the play's `roles:` plus any `import_role`, plus each `include_role` once it has run — not `meta/main.yml` dependencies; prefer `ansible_play_role_names`"),
    row!("environment", Always, GET_VARS, "the `environment:` keyword in effect, merged play → block → task, as a list of dicts"),
    row!("vars", Always, GET_VARS, "dict of every variable in effect, for a lookup by name"),
    row!("omit", Always, "the templar", "the sentinel that removes a module argument when passed as its value"),
    // --- unprefixed, scoped ---
    row!("item", Loop, LOOP, "the current loop item — unless `loop_control: loop_var` renames it, in which case `item` is undefined"),
    row!("role_name", Role, ROLE, "name of the role the current task belongs to"),
    row!("role_path", Role, ROLE, "directory of the role the current task belongs to"),
    row!("role_uuid", Role, ROLE, "an id ansible gives this role instance to tell duplicates apart; not meaningful to a play"),
    // --- ansible_*, every task ---
    row!("ansible_check_mode", Always, MAGIC, "whether the run is in `--check` mode"),
    row!("ansible_diff_mode", Always, MAGIC, "whether the run is in `--diff` mode"),
    row!("ansible_verbosity", Always, MAGIC, "the count of `-v` flags"),
    row!("ansible_version", Always, MAGIC, "the ansible-core version, as a dict (`full`, `major`, `minor`, `revision`, `string`)"),
    row!("ansible_playbook_python", Always, MAGIC, "the interpreter `ansible-playbook` itself runs on"),
    row!("ansible_config_file", Always, MAGIC, "the ansible.cfg in use, or none"),
    row!("ansible_facts", Always, GET_VARS, "the current host's facts, as a dict — filled by `setup` and by any module that returns `ansible_facts`"),
    row!("ansible_play_name", Always, MAGIC, "the current play's `name:`"),
    row!("ansible_play_hosts", Always, MAGIC, "hosts still active in the current play"),
    row!("ansible_play_hosts_all", Always, MAGIC, "every host the play was started against, failed ones included"),
    row!("ansible_play_batch", Always, MAGIC, "hosts in the current `serial:` batch"),
    row!("ansible_current_hosts", Always, MAGIC, "hosts still active in the current play"),
    row!("ansible_failed_hosts", Always, MAGIC, "hosts that have failed in the current play"),
    row!("ansible_play_role_names", Always, MAGIC, "names of the roles the play references directly (`roles:`, `import_role`, and each `include_role` once run)"),
    row!("ansible_role_names", Always, MAGIC, "`ansible_play_role_names` plus every `meta/main.yml` dependency"),
    row!("ansible_dependent_role_names", Always, MAGIC, "role names reached only through `meta/main.yml` dependencies"),
    row!("ansible_run_tags", Always, OPTIONS, "the `--tags` given"),
    row!("ansible_skip_tags", Always, OPTIONS, "the `--skip-tags` given"),
    row!("ansible_forks", Always, OPTIONS, "the `--forks` in effect"),
    row!("ansible_inventory_sources", Always, OPTIONS, "every `-i` and config inventory source"),
    row!("ansible_search_path", Always, MAGIC, "directories ansible searches for files, innermost first"),
    row!("ansible_connection", Always, PLAY_CONTEXT, "the connection plugin in effect for the current host"),
    row!("ansible_host", Always, PLAY_CONTEXT, "the address the connection plugin uses for the current host"),
    row!("ansible_ssh_host", Always, PLAY_CONTEXT, "older spelling of `ansible_host`"),
    row!("ansible_timeout", Always, PLAY_CONTEXT, "the connection timeout in effect"),
    row!("ansible_ssh_timeout", Always, PLAY_CONTEXT, "older spelling of `ansible_timeout`"),
    row!("ansible_pipelining", Always, PLAY_CONTEXT, "whether pipelining is in effect"),
    row!("ansible_ssh_pipelining", Always, PLAY_CONTEXT, "older spelling of `ansible_pipelining`"),
    row!("ansible_module_compression", Always, PLAY_CONTEXT, "how module payloads are compressed"),
    row!("ansible_shell_executable", Always, PLAY_CONTEXT, "the remote shell modules run under"),
    // --- ansible_*, scoped ---
    row!("ansible_loop_var", Loop, LOOP, "the name of the loop variable — `item`, or what `loop_control: loop_var` set"),
    row!("ansible_loop", ExtendedLoop, LOOP, "the extended loop info: `index`, `index0`, `first`, `last`, `length`, `previtem`, `nextitem`, `allitems`"),
    row!("ansible_index_var", IndexVar, LOOP, "the name `loop_control: index_var` chose"),
    row!("ansible_role_name", Role, ROLE, "fully qualified name of the role the current task belongs to"),
    row!("ansible_collection_name", Role, ROLE, "the collection the current role came from, or none"),
    row!("ansible_parent_role_names", ChildRole, ROLE, "names of the roles that included or imported this one, innermost first"),
    row!("ansible_parent_role_paths", ChildRole, ROLE, "directories of the roles that included or imported this one, innermost first"),
    row!("ansible_delegated_vars", Delegated, GET_VARS, "the delegated host's variables, keyed by its name"),
];

/// The table row for `name`, if ansible sets it.
pub fn injected(name: &str) -> Option<&'static Injected> {
    TABLE.iter().find(|r| r.name == name)
}

/// A name outside the table that could still be ansible's: the `ansible_<fact>` set exists
/// only after `setup` runs and is host-dependent, so no static reader can enumerate it. It
/// is also the prefix inventory connection variables use (`ansible_user`, `ansible_port`),
/// which are the user's to define, so the prefix means "may be provided", never "is".
pub fn may_be_fact(name: &str) -> bool {
    name.starts_with("ansible_")
}

/// Whether a read of `name` can be answered without any workspace definition: a table row,
/// in any scope, or a possible fact. This is the exemption the undefined rule and the
/// rule-facing use scanner apply; scope is not consulted here (T-224 slice 2).
pub fn provided(name: &str) -> bool {
    injected(name).is_some() || may_be_fact(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name the 2.21.2 `varnames` run listed at a plain play task, prefixed or not,
    /// minus `_task`. If a row goes missing, or a row is added that the run did not show,
    /// this is what notices.
    #[test]
    fn the_always_scope_is_exactly_what_a_play_task_sees() {
        let mut measured: Vec<&str> = "environment group_names groups hostvars inventory_dir \
             inventory_file inventory_hostname inventory_hostname_short play_hosts \
             playbook_dir role_names vars omit \
             ansible_check_mode ansible_config_file ansible_connection ansible_current_hosts \
             ansible_dependent_role_names ansible_diff_mode ansible_facts ansible_failed_hosts \
             ansible_forks ansible_host ansible_inventory_sources ansible_module_compression \
             ansible_pipelining ansible_play_batch ansible_play_hosts ansible_play_hosts_all \
             ansible_play_name ansible_play_role_names ansible_playbook_python \
             ansible_role_names ansible_run_tags ansible_search_path ansible_shell_executable \
             ansible_skip_tags ansible_ssh_host ansible_ssh_pipelining ansible_ssh_timeout \
             ansible_timeout ansible_verbosity ansible_version"
            .split_whitespace()
            .collect();
        measured.sort_unstable();
        let mut always: Vec<&str> = TABLE
            .iter()
            .filter(|r| r.scope == Scope::Always)
            .map(|r| r.name)
            .collect();
        always.sort_unstable();
        assert_eq!(always, measured);
    }

    #[test]
    fn the_scoped_rows_are_what_each_extra_context_added() {
        let of = |s: Scope| {
            let mut v: Vec<&str> = TABLE.iter().filter(|r| r.scope == s).map(|r| r.name).collect();
            v.sort_unstable();
            v
        };
        assert_eq!(of(Scope::Loop), ["ansible_loop_var", "item"]);
        assert_eq!(of(Scope::ExtendedLoop), ["ansible_loop"]);
        assert_eq!(of(Scope::IndexVar), ["ansible_index_var"]);
        assert_eq!(
            of(Scope::Role),
            ["ansible_collection_name", "ansible_role_name", "role_name", "role_path", "role_uuid"]
        );
        assert_eq!(of(Scope::ChildRole), ["ansible_parent_role_names", "ansible_parent_role_paths"]);
        assert_eq!(of(Scope::Delegated), ["ansible_delegated_vars"]);
    }

    #[test]
    fn no_name_is_listed_twice() {
        let mut names: Vec<&str> = TABLE.iter().map(|r| r.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), TABLE.len());
    }

    #[test]
    fn provided_is_the_table_or_the_fact_prefix_and_nothing_else() {
        assert!(provided("inventory_file"));
        assert!(provided("ansible_os_family"));
        assert!(provided("ansible_not_a_thing"), "the prefix is a maybe, kept until T-224 slice 3");
        assert!(!provided("nope_missing"));
        assert!(!provided("ansible"));
        assert!(!provided("_task"));
    }
}
