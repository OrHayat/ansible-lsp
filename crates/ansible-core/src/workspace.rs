//! Where a file sits in an Ansible project.

use crate::config::AnsibleConfig;
use std::sync::Arc;

use crate::fs::{Fs, Kind, StdFs};
use crate::install::AnsibleInstall;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct FileContext {
    /// Directory containing `ansible.cfg`, if one is found walking up.
    pub project_root: Option<PathBuf>,
    /// e.g. `.../roles/lustre-snapshot`
    pub role_dir: Option<PathBuf>,
    /// The role subdir Ansible anchors this file's includes at — NOT the including
    /// file's own directory, once includes nest. `handlers/` for a file under a role's
    /// handlers (tasks loaded from there are Handlers, and their includes anchor there:
    /// `included_file.py:172`), else `tasks/` (T-092).
    pub role_anchor_dir: Option<PathBuf>,
    /// The including file's own directory.
    pub file_dir: PathBuf,
    pub config: AnsibleConfig,
    /// The Ansible install this file resolves against (T-201 box 4).
    ///
    /// A value carried with the context rather than a process-wide `OnceLock`: the install is
    /// per-*server*, not per-process, and while a slot was indistinguishable from that in a
    /// single server it made "which install answered this" untestable and let any caller start
    /// the ~300 ms detection from a request path (T-084).
    ///
    /// `None` means detection has not run yet, and every reader must treat it as "not known
    /// yet" rather than "no Ansible": before startup finishes, builtin modules simply do not
    /// resolve, which is a missing answer and never a wrong one.
    pub install: Option<Arc<AnsibleInstall>>,
    /// Collection routing tables, shared with the cache that built this context (T-201 box 6).
    /// Default is an empty memo of its own — correct for a context with no cache behind it,
    /// which then parses what it needs and drops it.
    pub routing: Arc<crate::cache::RoutingTables>,
}

impl FileContext {
    pub fn discover(file: &Path) -> Self {
        Self::discover_with(file, &StdFs, AnsibleConfig::load)
    }

    /// [`discover`](Self::discover) with the `ansible.cfg` read supplied by the caller, so a
    /// scan can read each project's config once instead of once per file (T-076), and against
    /// a caller-supplied filesystem so the ancestor walk's probes can be memoized (T-085).
    /// Only the file's *directory* is looked at, which is why a cache can key on it.
    pub fn discover_with(
        file: &Path,
        fs: &dyn Fs,
        config: impl FnOnce(&Path) -> AnsibleConfig,
    ) -> Self {
        let file_dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
        let (role_dir, role_anchor_dir) = find_role(&file_dir, fs);
        let project_root = find_project_root(&file_dir, fs);
        // No ansible.cfg above the file still leaves the env and `~/.ansible` layers — a
        // galaxy role must resolve for a rootless playbook — so load with the file's own
        // directory standing in for the root rather than defaulting the whole config.
        let config = config(project_root.as_deref().unwrap_or(&file_dir));
        Self {
            project_root,
            role_dir,
            role_anchor_dir,
            file_dir,
            config,
            install: None,
            routing: Arc::default(),
        }
    }

    /// Attach the detected Ansible install. A builder rather than a `discover` parameter so
    /// the many callers that have no install — every test with no server behind it — keep
    /// saying so by omission instead of threading a `None`.
    pub fn with_install(mut self, install: Option<Arc<AnsibleInstall>>) -> Self {
        self.install = install;
        self
    }

    /// Share a cache's routing-table memo, so every context it builds parses each table once.
    pub fn with_routing(mut self, routing: Arc<crate::cache::RoutingTables>) -> Self {
        self.routing = routing;
        self
    }

    /// Base dirs for task includes, in Ansible's order.
    ///
    /// Role `tasks/` first: `tasks/query/timestamp.yml` includes `query/exists.yml`,
    /// which resolves only against `tasks/` — from the file's own dir it would be
    /// `tasks/query/query/exists.yml`. Getting this wrong invents missing-file
    /// warnings on working playbooks.
    pub fn task_search_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        push_unique(&mut dirs, self.role_anchor_dir.clone());
        push_unique(&mut dirs, self.handler_fallback_dir());
        push_unique(&mut dirs, self.role_dir.clone());
        push_unique(&mut dirs, Some(self.file_dir.clone()));
        push_unique(&mut dirs, self.project_root.clone());
        dirs
    }

    /// Where a relative `include_vars:` file is looked up, in order (T-206).
    ///
    /// Upstream is `_find_needle('vars', src)` -> `path_dwim_relative_stack`, which tries
    /// `<path>/vars/<src>` before `<path>/<src>` for each entry on the search stack — so the
    /// `vars/` subdir of a directory always beats the directory itself, and the role comes
    /// before the file's own dir. Measured on 2.21.3 by putting a copy at all five and
    /// deleting the winner until the list ran out.
    ///
    /// The order matters most where the two collide: a role's `tasks/main.yml` writing
    /// `include_vars: main.yml` means its own `vars/main.yml`, and searching `<file_dir>`
    /// first makes the lookup find the including file.
    pub fn include_vars_bases(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        push_unique(&mut dirs, self.role_dir.as_ref().map(|r| r.join("vars")));
        push_unique(&mut dirs, Some(self.file_dir.join("vars")));
        push_unique(&mut dirs, Some(self.file_dir.clone()));
        push_unique(&mut dirs, self.project_root.as_ref().map(|r| r.join("vars")));
        push_unique(&mut dirs, self.project_root.clone());
        dirs
    }

    /// For a file anchored at `handlers/`: the role's `tasks/`, tried after the anchor.
    /// A handler include that misses in `handlers/` legally falls back there —
    /// `path_dwim_relative`'s "look in role's tasks dir w/o dirname"
    /// (`dataloader.py:311-313`). None for a file anchored at `tasks/`.
    fn handler_fallback_dir(&self) -> Option<PathBuf> {
        let anchor = self.role_anchor_dir.as_deref()?;
        if anchor.file_name()? != "handlers" {
            return None;
        }
        Some(self.role_dir.as_deref()?.join("tasks"))
    }

    /// This file is the role's `meta/main.yml` — the one Ansible loads as `RoleMetadata`
    /// (`role/__init__.py:265`), not `meta/argument_specs.yml`, which sits in the same
    /// directory under a different schema (T-149).
    ///
    /// One predicate rather than one per caller: both the dependency extractor and T-147's
    /// key check ask this question, and a file is either RoleMetadata or it isn't.
    ///
    /// `_load_role_yaml` hard-codes `.yml .yaml .json` plus extensionless `main`
    /// (`role/__init__.py:422-428`). Only the first two can reach us — [`yaml_files_in`]
    /// collects no other extension — so widening this list buys nothing until the walker
    /// changes too.
    pub fn is_role_metadata(&self, path: &Path) -> bool {
        let Some(role) = &self.role_dir else { return false };
        path.parent().is_some_and(|d| d == role.join("meta"))
            && path.file_stem().is_some_and(|s| s == "main")
            && matches!(path.extension().and_then(|e| e.to_str()), Some("yml" | "yaml"))
    }

    /// Directories that contain roles, in search order.
    pub fn roles_roots(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        for p in self.config.roles_path.iter().flatten() {
            push_unique(&mut dirs, Some(p.clone()));
        }
        push_unique(&mut dirs, self.project_root.as_ref().map(|r| r.join("roles")));
        // A role may include a sibling role, so the current role's parent is a root.
        push_unique(
            &mut dirs,
            self.role_dir.as_ref().and_then(|r| r.parent()).map(Path::to_path_buf),
        );
        push_unique(&mut dirs, Some(self.file_dir.join("roles")));
        // Ansible's built-in defaults apply only when nothing set roles_path — a set
        // value replaces them rather than extending them, and set-but-empty counts as set.
        if self.config.roles_path.is_none() {
            push_unique(&mut dirs, self.config.ansible_home.as_ref().map(|h| h.join("roles")));
            for d in ["/usr/share/ansible/roles", "/etc/ansible/roles"] {
                push_unique(&mut dirs, Some(PathBuf::from(d)));
            }
        }
        dirs
    }

    /// Legacy module search dirs for a bare module name, in the loader's order
    /// (`loader.py:470-521`, `_get_paths_with_context`):
    ///
    /// 1. `extra_dirs` — `library/` dirs harvested while loading plays and roles
    ///    (`module_loader`'s subdir name, `loader.py:1799-1804`). Approximated here with
    ///    the file's own role, its directory, and the project root: the real set is
    ///    per-execution state (whatever loaded before the task) we can't replay.
    /// 2. the `library` cfg key when set, else `DEFAULT_MODULE_PATH`'s defaults
    ///    `~/.ansible/plugins/modules` and `/usr/share/ansible/plugins/modules`
    ///    (`config/base.yml:945-951` — the key *replaces* the defaults).
    ///
    /// The builtin package tree comes after all of these — "package path always gets
    /// added last so that every other type of path is searched before it" — and is
    /// appended by the caller, which also owns the routing-table last-ditch step.
    pub fn legacy_module_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(role) = &self.role_dir {
            push_unique(&mut dirs, Some(role.join("library")));
        }
        push_unique(&mut dirs, Some(self.file_dir.join("library")));
        push_unique(&mut dirs, self.project_root.as_ref().map(|r| r.join("library")));
        match &self.config.library {
            None => {
                push_unique(
                    &mut dirs,
                    self.config.ansible_home.as_ref().map(|h| h.join("plugins/modules")),
                );
                push_unique(&mut dirs, Some(PathBuf::from("/usr/share/ansible/plugins/modules")));
            }
            Some(list) => {
                for p in list {
                    push_unique(&mut dirs, Some(p.clone()));
                }
            }
        }
        dirs
    }

    /// Where a controller-side action plugin can legacy-override a same-named module — the
    /// pre-collections search Ansible still honors (`DEFAULT_ACTION_PLUGIN_PATH`): a role's
    /// own `action_plugins/`, dirs adjacent to the file and project, then the
    /// `action_plugins` cfg key (or its `~/.ansible` + `/usr/share` defaults). Mirrors
    /// [`legacy_module_dirs`] with the `action_plugins` subdir. (T-073)
    pub fn legacy_action_plugin_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(role) = &self.role_dir {
            push_unique(&mut dirs, Some(role.join("action_plugins")));
        }
        push_unique(&mut dirs, Some(self.file_dir.join("action_plugins")));
        push_unique(&mut dirs, self.project_root.as_ref().map(|r| r.join("action_plugins")));
        match &self.config.action_plugins {
            None => {
                push_unique(
                    &mut dirs,
                    self.config.ansible_home.as_ref().map(|h| h.join("plugins/action")),
                );
                push_unique(&mut dirs, Some(PathBuf::from("/usr/share/ansible/plugins/action")));
            }
            Some(list) => {
                for p in list {
                    push_unique(&mut dirs, Some(p.clone()));
                }
            }
        }
        dirs
    }

    pub fn collection_roots(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        for p in self.config.collections_path.iter().flatten() {
            push_unique(&mut dirs, Some(p.join("ansible_collections")));
        }
        push_unique(
            &mut dirs,
            self.project_root
                .as_ref()
                .map(|r| r.join("collections/ansible_collections")),
        );
        // Collections you installed rather than wrote — still worth navigating into.
        // `detected`, not a detection: this runs on every hover and jump, and starting the
        // ~300 ms probe here is the freeze T-084 measured. Before startup has detected, an
        // installed collection simply is not offered yet.
        for r in self.install.iter().flat_map(|i| &i.collection_roots) {
            push_unique(&mut dirs, Some(r.clone()));
        }
        dirs
    }
}

/// Every YAML file under `root`, skipping VCS and cache directories.
pub fn yaml_files(root: &Path) -> Vec<PathBuf> {
    yaml_files_in(root, &StdFs)
}

/// [`yaml_files`] against a caller-supplied filesystem (T-085).
pub fn yaml_files_in(root: &Path, fs: &dyn Fs) -> Vec<PathBuf> {
    fn walk(dir: &Path, fs: &dyn Fs, out: &mut Vec<PathBuf>) {
        // The kind comes from the listing itself — no stat per entry.
        for (p, kind) in fs.read_dir(dir) {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if kind == Kind::Dir {
                if !matches!(
                    name.as_str(),
                    ".git" | "__pycache__" | ".pytest_cache" | "node_modules" | "target"
                ) {
                    walk(&p, fs, out);
                }
            } else if matches!(
                p.extension().and_then(|s| s.to_str()),
                Some("yml") | Some("yaml")
            ) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, fs, &mut out);
    out
}

fn push_unique(dirs: &mut Vec<PathBuf>, p: Option<PathBuf>) {
    if let Some(p) = p {
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    }
}

/// Walks up asking "is `ansible.cfg` here?" at every ancestor.
///
/// This walk is an **editor heuristic, not Ansible behaviour**. Real discovery
/// (`find_ini_config_file`, `manager.py:253-313`) checks the invocation CWD only — no
/// ancestor walk. But a language server has no meaningful CWD, so the walk predicts the
/// one the user will run from: open `~/app/ansible/playbooks/site.yml` in a workspace
/// rooted at `~/app`, and the cfg that governs their real run is
/// `~/app/ansible/ansible.cfg`, because that is where they will `cd` to invoke
/// `ansible-playbook`. The nearest enclosing cfg *is* that prediction — anchored to the
/// file, never to the workspace, so a monorepo of several ansible trees resolves each
/// file against its own config. (T-098)
///
/// Each *directory* asks about the same shared ancestors, so on a scan this is the single
/// most repeated probe in the codebase — 28 hits on one path in a 60-file demo. A
/// memoizing `fs` collapses it (T-085).
fn find_project_root(from: &Path, fs: &dyn Fs) -> Option<PathBuf> {
    from.ancestors()
        .find(|d| fs.is_file(&d.join("ansible.cfg")))
        .map(Path::to_path_buf)
}

/// The role a file belongs to and the subdir its includes anchor at.
fn find_role(from: &Path, fs: &dyn Fs) -> (Option<PathBuf>, Option<PathBuf>) {
    // Under tasks/ or handlers/: the anchor is that dir itself — Ansible picks it by the
    // including task's kind, `'handlers' if isinstance(original_task, Handler) else
    // 'tasks'` (included_file.py:172), which for role files means the dir they load from.
    if let Some(anchor) = from.ancestors().find(|d| is_include_anchor(d, fs)) {
        return (anchor.parent().map(Path::to_path_buf), Some(anchor.to_path_buf()));
    }
    // Elsewhere in a role (vars/, meta/, ...): no task lists live there, so includes keep
    // the conventional tasks/ anchor.
    if let Some(role) = from.ancestors().find(|d| is_role_dir(d, fs)) {
        return (Some(role.to_path_buf()), Some(role.join("tasks")));
    }
    (None, None)
}

/// A role's `tasks/` or `handlers/` dir — the only two role subdirs that hold task
/// lists, and therefore the only two places an include can anchor.
fn is_include_anchor(dir: &Path, fs: &dyn Fs) -> bool {
    matches!(dir.file_name().and_then(|n| n.to_str()), Some("tasks" | "handlers"))
        && dir.parent().is_some_and(|role| is_role_dir(role, fs))
}

fn is_role_dir(d: &Path, fs: &dyn Fs) -> bool {
    fs.is_dir(&d.join("tasks")) || fs.is_dir(&d.join("defaults")) || fs.is_dir(&d.join("meta"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-098. The `.ansible` halves of the default search dirs follow `ansible_home`
    /// rather than hardcoding `$HOME/.ansible`.
    #[test]
    fn ansible_home_relocates_the_default_search_dirs() {
        use crate::config::{AnsibleConfig, EnvMap};
        let fs = crate::testing::MemFs::new(&[
            ("/p/ansible.cfg", "[defaults]\nhome = /opt/ans\n"),
            ("/p/play.yml", ""),
        ]);
        let env = EnvMap::from_pairs(&[("HOME", "/home/t")]);
        let ctx = FileContext::discover_with(Path::new("/p/play.yml"), &fs, |root| {
            AnsibleConfig::builder(root).fs(&fs).env(&env).load()
        });

        assert!(ctx.roles_roots().contains(&PathBuf::from("/opt/ans/roles")));
        assert!(ctx.legacy_module_dirs().contains(&PathBuf::from("/opt/ans/plugins/modules")));
        assert!(ctx
            .legacy_action_plugin_dirs()
            .contains(&PathBuf::from("/opt/ans/plugins/action")));
        let all: Vec<_> = ctx
            .roles_roots()
            .into_iter()
            .chain(ctx.legacy_module_dirs())
            .chain(ctx.legacy_action_plugin_dirs())
            .collect();
        assert!(
            !all.iter().any(|p| p.starts_with("/home/t")),
            "the relocated home fully replaces `$HOME/.ansible`: {all:?}"
        );
    }

    /// A file with no ansible.cfg anywhere above it still searches the env-derived home
    /// dirs — a galaxy role in `~/.ansible/roles` must resolve for a rootless playbook.
    #[test]
    fn a_rootless_file_still_gets_the_home_derived_default_dirs() {
        use crate::config::{AnsibleConfig, EnvMap};
        let fs = crate::testing::MemFs::new(&[("/nowhere/play.yml", "")]);
        let env = EnvMap::from_pairs(&[("HOME", "/home/t")]);
        let ctx = FileContext::discover_with(Path::new("/nowhere/play.yml"), &fs, |root| {
            AnsibleConfig::builder(root).fs(&fs).env(&env).load()
        });
        assert!(ctx.project_root.is_none(), "fixture really is rootless");
        assert!(ctx.roles_roots().contains(&PathBuf::from("/home/t/.ansible/roles")));
        assert!(ctx
            .legacy_module_dirs()
            .contains(&PathBuf::from("/home/t/.ansible/plugins/modules")));
    }

    /// An explicitly emptied path list disables the built-in defaults instead of
    /// resurrecting them — set-but-empty is set.
    #[test]
    fn an_explicitly_emptied_roles_path_disables_the_default_dirs() {
        use crate::config::{AnsibleConfig, EnvMap};
        let fs = crate::testing::MemFs::new(&[("/p/ansible.cfg", ""), ("/p/play.yml", "")]);
        let env =
            EnvMap::from_pairs(&[("HOME", "/home/t"), ("ANSIBLE_ROLES_PATH", "")]);
        let ctx = FileContext::discover_with(Path::new("/p/play.yml"), &fs, |root| {
            AnsibleConfig::builder(root).fs(&fs).env(&env).load()
        });
        let roots = ctx.roles_roots();
        assert!(
            !roots.contains(&PathBuf::from("/home/t/.ansible/roles"))
                && !roots.contains(&PathBuf::from("/usr/share/ansible/roles")),
            "user disabled the defaults; they must not come back: {roots:?}"
        );
    }

    /// A task file two levels inside a role's `tasks/` must still anchor at `tasks/`, not at
    /// its own directory — otherwise `query/exists.yml` resolves to
    /// `tasks/query/query/exists.yml` and the tool invents a missing-file warning on a
    /// working playbook (T-092).
    ///
    /// Built inline, not read from `$HOME`: the home-dir version skipped silently for anyone
    /// without that private repo — so it asserted nothing on a clean checkout — and panicked
    /// outright on Windows, where `HOME` is unset (T-077).
    #[test]
    fn nested_task_file_resolves_against_role_tasks_dir() {
        // `roles_path` is set explicitly: the conventional `roles/` dir alone would satisfy
        // the last assertion, so without it that line would prove nothing.
        let root = crate::testing::project(
            "nested-role",
            "[defaults]\nroles_path = ./roles\n",
            &[("roles/snapshot/tasks/query/timestamp.yml", "")],
        );
        let f = root.join("roles/snapshot/tasks/query/timestamp.yml");

        let c = FileContext::discover(&f);
        assert!(c.role_dir.as_ref().unwrap().ends_with("roles/snapshot"));
        assert!(
            c.role_anchor_dir
                .as_ref()
                .unwrap()
                .ends_with("roles/snapshot/tasks"),
            "anchored at {:?}, not the role's tasks/",
            c.role_anchor_dir
        );
        assert!(c.project_root.as_ref().unwrap().ends_with("ansible-lsp-nested-role"));
        assert!(c.roles_roots().iter().any(|r| r.ends_with("ansible-lsp-nested-role/roles")));
    }
}
