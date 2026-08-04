//! Where a file sits in an Ansible project.

use crate::config::AnsibleConfig;
use crate::fs::{Fs, Kind, StdFs};
use crate::install::AnsibleInstall;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct FileContext {
    /// Directory containing `ansible.cfg`, if one is found walking up.
    pub project_root: Option<PathBuf>,
    /// e.g. `.../roles/lustre-snapshot`
    pub role_dir: Option<PathBuf>,
    /// The base Ansible resolves task includes against — NOT the including file's
    /// own directory, once includes nest.
    pub role_tasks_dir: Option<PathBuf>,
    /// The including file's own directory.
    pub file_dir: PathBuf,
    pub config: AnsibleConfig,
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
        let (role_dir, role_tasks_dir) = find_role(&file_dir, fs);
        let project_root = find_project_root(&file_dir, fs);
        let config = project_root.as_deref().map(config).unwrap_or_default();
        Self {
            project_root,
            role_dir,
            role_tasks_dir,
            file_dir,
            config,
        }
    }

    /// Base dirs for task includes, in Ansible's order.
    ///
    /// Role `tasks/` first: `tasks/query/timestamp.yml` includes `query/exists.yml`,
    /// which resolves only against `tasks/` — from the file's own dir it would be
    /// `tasks/query/query/exists.yml`. Getting this wrong invents missing-file
    /// warnings on working playbooks.
    pub fn task_search_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        push_unique(&mut dirs, self.role_tasks_dir.clone());
        push_unique(&mut dirs, self.role_dir.clone());
        push_unique(&mut dirs, Some(self.file_dir.clone()));
        push_unique(&mut dirs, self.project_root.clone());
        dirs
    }

    /// Directories that contain roles, in search order.
    pub fn roles_roots(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        for p in &self.config.roles_path {
            push_unique(&mut dirs, Some(p.clone()));
        }
        push_unique(&mut dirs, self.project_root.as_ref().map(|r| r.join("roles")));
        // A role may include a sibling role, so the current role's parent is a root.
        push_unique(
            &mut dirs,
            self.role_dir.as_ref().and_then(|r| r.parent()).map(Path::to_path_buf),
        );
        push_unique(&mut dirs, Some(self.file_dir.join("roles")));
        // Ansible's built-in defaults apply only when ansible.cfg doesn't set
        // roles_path — the config key replaces them rather than extending them.
        if self.config.roles_path.is_empty() {
            for d in ["~/.ansible/roles", "/usr/share/ansible/roles", "/etc/ansible/roles"] {
                push_unique(&mut dirs, expand_home(d));
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
        if self.config.library.is_empty() {
            if let Ok(home) = std::env::var("HOME") {
                push_unique(&mut dirs, Some(PathBuf::from(home).join(".ansible/plugins/modules")));
            }
            push_unique(&mut dirs, Some(PathBuf::from("/usr/share/ansible/plugins/modules")));
        } else {
            for p in &self.config.library {
                push_unique(&mut dirs, Some(p.clone()));
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
        if self.config.action_plugins.is_empty() {
            if let Ok(home) = std::env::var("HOME") {
                push_unique(&mut dirs, Some(PathBuf::from(home).join(".ansible/plugins/action")));
            }
            push_unique(&mut dirs, Some(PathBuf::from("/usr/share/ansible/plugins/action")));
        } else {
            for p in &self.config.action_plugins {
                push_unique(&mut dirs, Some(p.clone()));
            }
        }
        dirs
    }

    pub fn collection_roots(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        for p in &self.config.collections_path {
            push_unique(&mut dirs, Some(p.join("ansible_collections")));
        }
        push_unique(
            &mut dirs,
            self.project_root
                .as_ref()
                .map(|r| r.join("collections/ansible_collections")),
        );
        // Collections you installed rather than wrote — still worth navigating into.
        for r in &AnsibleInstall::detect().collection_roots {
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

fn expand_home(p: &str) -> Option<PathBuf> {
    match p.strip_prefix("~/") {
        Some(rest) => std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(rest)),
        None => Some(PathBuf::from(p)),
    }
}

fn push_unique(dirs: &mut Vec<PathBuf>, p: Option<PathBuf>) {
    if let Some(p) = p {
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    }
}

/// Walks up asking "is `ansible.cfg` here?" at every ancestor. Each *directory* asks about
/// the same shared ancestors, so on a scan this is the single most repeated probe in the
/// codebase — 28 hits on one path in a 60-file demo. A memoizing `fs` collapses it (T-085).
fn find_project_root(from: &Path, fs: &dyn Fs) -> Option<PathBuf> {
    from.ancestors()
        .find(|d| fs.is_file(&d.join("ansible.cfg")))
        .map(Path::to_path_buf)
}

fn find_role(from: &Path, fs: &dyn Fs) -> (Option<PathBuf>, Option<PathBuf>) {
    for dir in from.ancestors() {
        if dir.file_name().and_then(|n| n.to_str()) == Some("tasks") {
            if let Some(role) = dir.parent() {
                if is_role_dir(role, fs) {
                    return (Some(role.to_path_buf()), Some(dir.to_path_buf()));
                }
            }
        }
    }
    for dir in from.ancestors() {
        if is_role_dir(dir, fs) {
            return (Some(dir.to_path_buf()), Some(dir.join("tasks")));
        }
    }
    (None, None)
}

fn is_role_dir(d: &Path, fs: &dyn Fs) -> bool {
    fs.is_dir(&d.join("tasks")) || fs.is_dir(&d.join("defaults")) || fs.is_dir(&d.join("meta"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_task_file_resolves_against_role_tasks_dir() {
        let home = std::env::var("HOME").unwrap();
        let f = format!("{home}/app/ansible/roles/lustre-snapshot/tasks/query/timestamp.yml");
        if !Path::new(&f).exists() {
            return;
        }
        let c = FileContext::discover(Path::new(&f));
        assert!(c.role_dir.as_ref().unwrap().ends_with("roles/lustre-snapshot"));
        assert!(
            c.role_tasks_dir
                .as_ref()
                .unwrap()
                .ends_with("roles/lustre-snapshot/tasks")
        );
        assert!(c.project_root.as_ref().unwrap().ends_with("app/ansible"));
        // roles_path from ansible.cfg must be in play, not just the conventional dir.
        assert!(c.roles_roots().iter().any(|r| r.ends_with("ansible/roles")));
    }
}
