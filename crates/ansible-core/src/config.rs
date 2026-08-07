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
    /// What a duplicate YAML mapping key does. `DUPLICATE_YAML_DICT_KEY`'s ini name is
    /// `duplicate_dict_key`; the env var keeps the longer spelling. T-102.
    pub duplicate_dict_key: DuplicateDictKey,
}

/// `DUPLICATE_YAML_DICT_KEY` (`config/base.yml:1361-1375`). Exactly three values, lowercase
/// — live-verified on ansible-core 2.21.2, where `False` (which the option's own
/// description still suggests), `IGNORE` and any other spelling abort every ansible command
/// with `Invalid value ... Valid values are: error, warn, ignore`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DuplicateDictKey {
    /// Warn and keep the last value. Ansible's default.
    #[default]
    Warn,
    /// Refuse to load the file; the play never starts.
    Error,
    /// Keep the last value silently.
    Ignore,
}

impl DuplicateDictKey {
    /// `None` for a spelling Ansible would reject. Callers fall back to the default rather
    /// than refusing to serve — a language server that goes dark over one bad config line
    /// is worse than one that analyses with the shipped default.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "warn" => Some(Self::Warn),
            "error" => Some(Self::Error),
            "ignore" => Some(Self::Ignore),
            _ => None,
        }
    }
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
                    "duplicate_dict_key" => {
                        if let Some(v) = DuplicateDictKey::parse(value.trim()) {
                            cfg.duplicate_dict_key = v;
                        }
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
        // Note the spelling: the env var is DUPLICATE_YAML_DICT_KEY, the ini key is not.
        if let Some(v) = std::env::var("ANSIBLE_DUPLICATE_YAML_DICT_KEY")
            .ok()
            .and_then(|v| DuplicateDictKey::parse(&v))
        {
            cfg.duplicate_dict_key = v;
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
    use crate::testing::CfgFs;

    fn cfg(text: &str) -> AnsibleConfig {
        AnsibleConfig::load_in(Path::new("/p"), &CfgFs::some(text))
    }

    /// T-102. All three spellings, plus the two ways a value can be absent. Live-verified
    /// against ansible-core 2.21.2 — see [`DuplicateDictKey`] for what it rejects.
    #[test]
    fn duplicate_dict_key_reads_all_three_values() {
        if std::env::var("ANSIBLE_DUPLICATE_YAML_DICT_KEY").is_ok() {
            return; // the env layer wins over everything asserted here
        }
        use DuplicateDictKey::*;
        // Unset anywhere, and set in another section, both mean Ansible's default.
        assert_eq!(cfg("[defaults]\nroles_path = ./roles\n").duplicate_dict_key, Warn);
        assert_eq!(cfg("[galaxy]\nduplicate_dict_key = error\n").duplicate_dict_key, Warn);

        assert_eq!(cfg("[defaults]\nduplicate_dict_key = warn\n").duplicate_dict_key, Warn);
        assert_eq!(cfg("[defaults]\nduplicate_dict_key = error\n").duplicate_dict_key, Error);
        assert_eq!(cfg("[defaults]\nduplicate_dict_key = ignore\n").duplicate_dict_key, Ignore);
    }

    /// Ansible aborts on a bad value; we cannot go dark over one config line, so the
    /// shipped default stands. The divergence is deliberate — a server that refuses to
    /// analyse is worse than one that analyses with `warn`.
    #[test]
    fn an_invalid_duplicate_dict_key_falls_back_rather_than_failing() {
        if std::env::var("ANSIBLE_DUPLICATE_YAML_DICT_KEY").is_ok() {
            return;
        }
        // `False` is what the option's own description suggests, and Ansible rejects it.
        for bad in ["False", "false", "IGNORE", "Error", "bogus", ""] {
            let text = format!("[defaults]\nduplicate_dict_key = {bad}\n");
            assert_eq!(
                cfg(&text).duplicate_dict_key,
                DuplicateDictKey::Warn,
                "{bad:?} should fall back to the default"
            );
        }
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

    /// A real-shaped config off the real filesystem, not the `CfgFs` stub: the `~` and `./`
    /// expansions are the point, and they run against actual paths. Built inline rather than
    /// read from a private repo under `$HOME` (T-077); `~` still needs `HOME`, so that one
    /// assertion — and only it — is skipped where the variable is unset, as on Windows.
    #[test]
    fn a_colon_list_expands_tilde_and_dot_against_the_project_root() {
        let root = crate::testing::project(
            "cfg-colon-list",
            "[defaults]\nroles_path = ~/ansible/roles:./roles\ncollections_path = ./collections\n",
            &[("roles/.keep", ""), ("collections/.keep", "")],
        );
        let cfg = AnsibleConfig::load(&root);

        assert_eq!(cfg.roles_path.len(), 2, "a colon splits entries: {:?}", cfg.roles_path);
        assert_eq!(cfg.roles_path[1], root.join("roles"), "`./` is project-root relative");
        assert!(cfg.collections_path.contains(&root.join("collections")));

        if let Ok(home) = std::env::var("HOME") {
            assert_eq!(cfg.roles_path[0], PathBuf::from(home).join("ansible/roles"));
        }
    }
}
