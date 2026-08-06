//! `ansible.cfg` — where roles and collections are searched for.

use std::path::{Path, PathBuf};

use crate::fs::{Fs, StdFs};

#[derive(Debug, Clone, Default)]
pub struct AnsibleConfig {
    pub roles_path: Vec<PathBuf>,
    pub collections_path: Vec<PathBuf>,
    /// The `library` key — `DEFAULT_MODULE_PATH`'s ini name (`config/base.yml:945-951`).
    /// When set it *replaces* the default legacy module dirs, not appends.
    pub library: Vec<PathBuf>,
    /// The `action_plugins` key — `DEFAULT_ACTION_PLUGIN_PATH`'s ini name. Legacy
    /// controller-side plugin dirs; a plugin here overrides a same-named module.
    pub action_plugins: Vec<PathBuf>,
    /// The `network_group_modules` key (`config/base.yml:1779-1788`). `None` means the key
    /// was never set, so [`DEFAULT_NETWORK_GROUP_MODULES`] applies; `Some(vec![])` is a
    /// deliberate "no platforms". Like every list key it *replaces* the default rather
    /// than extending it, so the two cases can't be collapsed into an empty `Vec`.
    pub network_group_modules: Option<Vec<String>>,
}

/// `NETWORK_GROUP_MODULES`' shipped default (`config/base.yml:1779-1788`). Network device
/// modules get one action plugin per *platform* — named for the prefix before the first
/// `_` — instead of the usual same-name twin, because the plugin holds the persistent
/// device connection and nothing can be shipped to a switch. (T-072)
pub const DEFAULT_NETWORK_GROUP_MODULES: &[&str] = &[
    "eos", "nxos", "ios", "iosxr", "junos", "enos", "ce", "vyos", "sros", "dellos9",
    "dellos10", "dellos6", "asa", "aruba", "aireos", "bigip", "ironware", "onyx", "netconf",
    "exos", "voss", "slxos",
];

impl AnsibleConfig {
    pub fn load(project_root: &Path) -> Self {
        Self::load_in(project_root, &StdFs)
    }

    /// [`load`](Self::load) against a caller-supplied filesystem, so a scan reads each
    /// project's config once (T-085).
    pub fn load_in(project_root: &Path, fs: &dyn Fs) -> Self {
        let mut cfg = Self::default();
        if let Some(text) = fs.read(&project_root.join("ansible.cfg")) {
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
                    "action_plugins" => cfg.action_plugins = paths(),
                    "network_group_modules" => {
                        cfg.network_group_modules = Some(name_list(value.trim()));
                    }
                    _ => {}
                }
            }
        }
        // Env beats the ini file, which beats the shipped default — and it applies whether
        // or not an `ansible.cfg` was found, so it can't ride inside the block above. Read
        // once here rather than at each lookup: a server's environment is fixed at launch,
        // so there is nothing to observe later.
        if let Ok(v) = std::env::var("ANSIBLE_NETWORK_GROUP_MODULES") {
            cfg.network_group_modules = Some(name_list(&v));
        }
        cfg
    }

    /// Whether `prefix` names a network platform, whose single action plugin handles every
    /// `<prefix>_*` module in its collection. (T-072)
    pub fn is_network_platform(&self, prefix: &str) -> bool {
        match &self.network_group_modules {
            Some(list) => list.iter().any(|p| p == prefix),
            None => DEFAULT_NETWORK_GROUP_MODULES.contains(&prefix),
        }
    }
}

/// A generic `type: list` value: comma-separated plain names. Unlike the path keys, which
/// are colon-separated and expanded — feeding one of these through [`expand_list`] would
/// split on the wrong character and then treat each name as a relative path.
fn name_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
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

    /// One `ansible.cfg` and nothing else — all `load_in` ever reads.
    struct CfgFs(String);

    impl Fs for CfgFs {
        fn kind(&self, _p: &Path) -> Option<crate::fs::Kind> {
            Some(crate::fs::Kind::File)
        }
        fn read(&self, _p: &Path) -> Option<String> {
            Some(self.0.clone())
        }
        fn read_dir(&self, _p: &Path) -> Vec<(PathBuf, crate::fs::Kind)> {
            Vec::new()
        }
        fn walk(&self, _root: &Path) -> Vec<(PathBuf, Vec<String>)> {
            Vec::new()
        }
        fn canonical(&self, p: &Path) -> Option<PathBuf> {
            Some(p.to_path_buf())
        }
    }

    fn cfg(text: &str) -> AnsibleConfig {
        AnsibleConfig::load_in(Path::new("/p"), &CfgFs(text.into()))
    }

    /// T-072. The env override is exercised by its own integration test, which needs a
    /// process to itself; this covers the two layers below it.
    #[test]
    fn network_group_modules_falls_back_then_is_replaced_by_the_cfg_key() {
        if std::env::var("ANSIBLE_NETWORK_GROUP_MODULES").is_ok() {
            return; // the env layer wins over everything asserted here
        }
        // Unset: the shipped platforms apply.
        let c = cfg("[defaults]\nroles_path = ./roles\n");
        assert_eq!(c.network_group_modules, None);
        assert!(c.is_network_platform("ios"));
        assert!(c.is_network_platform("slxos"));
        assert!(!c.is_network_platform("link"));

        // Set: a *comma* list — the colon that separates path keys is not a separator here,
        // so a single entry survives intact rather than splitting into two.
        let c = cfg("[defaults]\nnetwork_group_modules = ios, myos\n");
        assert!(c.is_network_platform("ios"));
        assert!(c.is_network_platform("myos"), "a site can add its own platform");
        assert!(
            !c.is_network_platform("nxos"),
            "the key replaces the defaults; it does not extend them"
        );

        // Present but empty is a deliberate "no platforms", which is why the field is an
        // Option — an empty Vec would be indistinguishable from the key being absent.
        let c = cfg("[defaults]\nnetwork_group_modules =\n");
        assert_eq!(c.network_group_modules, Some(Vec::new()));
        assert!(!c.is_network_platform("ios"));
    }

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
