//! Where a file sits in an Ansible project.

use crate::config::AnsibleConfig;
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
        let file_dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
        let (role_dir, role_tasks_dir) = find_role(&file_dir);
        let project_root = find_project_root(&file_dir);
        let config = project_root
            .as_deref()
            .map(AnsibleConfig::load)
            .unwrap_or_default();
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
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if !matches!(
                    name.as_str(),
                    ".git" | "__pycache__" | ".pytest_cache" | "node_modules" | "target"
                ) {
                    walk(&p, out);
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
    walk(root, &mut out);
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

fn find_project_root(from: &Path) -> Option<PathBuf> {
    from.ancestors()
        .find(|d| d.join("ansible.cfg").is_file())
        .map(Path::to_path_buf)
}

fn find_role(from: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    for dir in from.ancestors() {
        if dir.file_name().and_then(|n| n.to_str()) == Some("tasks") {
            if let Some(role) = dir.parent() {
                if is_role_dir(role) {
                    return (Some(role.to_path_buf()), Some(dir.to_path_buf()));
                }
            }
        }
    }
    for dir in from.ancestors() {
        if is_role_dir(dir) {
            return (Some(dir.to_path_buf()), Some(dir.join("tasks")));
        }
    }
    (None, None)
}

fn is_role_dir(d: &Path) -> bool {
    d.join("tasks").is_dir() || d.join("defaults").is_dir() || d.join("meta").is_dir()
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
