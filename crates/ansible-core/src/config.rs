//! `ansible.cfg` — where roles and collections are searched for.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct AnsibleConfig {
    pub roles_path: Vec<PathBuf>,
    pub collections_path: Vec<PathBuf>,
    /// The `library` key — `DEFAULT_MODULE_PATH`'s ini name (`config/base.yml:945-951`).
    /// When set it *replaces* the default legacy module dirs, not appends.
    pub library: Vec<PathBuf>,
}

impl AnsibleConfig {
    pub fn load(project_root: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(project_root.join("ansible.cfg")) else {
            return Self::default();
        };
        let mut cfg = Self::default();
        let mut in_defaults = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_defaults = line == "[defaults]";
                continue;
            }
            if !in_defaults || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let paths = || expand_list(value.trim(), project_root);
            match key.trim() {
                "roles_path" => cfg.roles_path = paths(),
                "collections_path" | "collections_paths" => cfg.collections_path = paths(),
                "library" => cfg.library = paths(),
                _ => {}
            }
        }
        cfg
    }
}

/// Colon-separated list; `~` expanded, relative entries resolved against the config's
/// own directory (Ansible resolves them against cwd, which is where you run it from).
fn expand_list(value: &str, base: &Path) -> Vec<PathBuf> {
    value
        .split(':')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| match s.strip_prefix("~/") {
            Some(rest) => match std::env::var("HOME") {
                Ok(home) => PathBuf::from(home).join(rest),
                Err(_) => PathBuf::from(s),
            },
            None if Path::new(s).is_absolute() => PathBuf::from(s),
            None => base.join(s.trim_start_matches("./")),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_real_repo_config() {
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let root = PathBuf::from(&home).join("app/ansible");
        if !root.join("ansible.cfg").is_file() {
            return;
        }
        let cfg = AnsibleConfig::load(&root);
        // `roles_path = ~/ansible/roles:./roles`
        assert_eq!(cfg.roles_path.len(), 2);
        assert_eq!(cfg.roles_path[0], PathBuf::from(&home).join("ansible/roles"));
        assert_eq!(cfg.roles_path[1], root.join("roles"));
        assert!(cfg.collections_path.contains(&root.join("collections")));
    }
}
