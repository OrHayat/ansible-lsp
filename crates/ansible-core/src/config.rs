//! `ansible.cfg` — where roles and collections are searched for.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::fs::{Fs, StdFs};

/// The process environment, snapshotted once — the seam that does for env vars what [`Fs`]
/// does for the filesystem: tests pass a literal map, and only
/// [`from_process`](Self::from_process) touches the real thing, so no test needs `set_var`
/// or a single-test binary to control what config sees. A server's environment is fixed at
/// launch, so a snapshot loses nothing.
pub struct EnvMap(HashMap<String, String>);

impl EnvMap {
    pub fn from_process() -> Self {
        Self(std::env::vars().collect())
    }

    pub fn empty() -> Self {
        Self(HashMap::new())
    }

    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Self(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    pub fn var(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }
}

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
    /// Where `~/.ansible` actually is (`ANSIBLE_HOME`, `base.yml:95-104`): env → the `home`
    /// ini key → `~/.ansible`. Every path default below is templated on it, which is why the
    /// hardcoded `.ansible` dirs in `workspace.rs` read this instead of `HOME`. `None` when
    /// nothing resolves it — no setting and no `HOME`, as on Windows.
    pub ansible_home: Option<PathBuf>,
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
        Self::builder(project_root).load()
    }

    /// [`load`](Self::load) with either dependency swappable: a caller-supplied filesystem
    /// so a scan reads each project's config once (T-085), a caller-supplied environment so
    /// a test controls what config sees.
    pub fn builder(project_root: &Path) -> AnsibleConfigBuilder<'_> {
        AnsibleConfigBuilder { root: project_root, fs: None, env: None }
    }

    fn load_resolved(project_root: &Path, fs: &dyn Fs, env: &EnvMap) -> Self {
        let mut cfg = Self::default();
        // `ANSIBLE_CONFIG` beats the walk-found project file — the front of
        // `find_ini_config_file`'s ladder (`manager.py:263-271`). First hit wins whole:
        // when the env file exists the project's is not read, let alone merged.
        let file = env_config_file(fs, env).unwrap_or_else(|| project_root.join("ansible.cfg"));
        let base = file.parent().unwrap_or(project_root);
        let mut home_key = None;
        if let Some(text) = fs.read(&file) {
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
                let paths = || expand_list(value.trim(), base, env);
                match key.trim() {
                    "home" => home_key = Some(value.trim().to_string()),
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
        // or not an `ansible.cfg` was found, so it can't ride inside the block above.
        if let Some(v) = env.var("ANSIBLE_ROLES_PATH") {
            cfg.roles_path = expand_list(v, base, env);
        }
        if let Some(v) = env.var("ANSIBLE_COLLECTIONS_PATH") {
            cfg.collections_path = expand_list(v, base, env);
        }
        if let Some(v) = env.var("ANSIBLE_LIBRARY") {
            cfg.library = expand_list(v, base, env);
        }
        cfg.ansible_home = env
            .var("ANSIBLE_HOME")
            .map(|v| expand_path(v, base, env))
            .or_else(|| home_key.map(|v| expand_path(&v, base, env)))
            .or_else(|| env.var("HOME").map(|h| PathBuf::from(h).join(".ansible")));
        if let Some(v) = env.var("ANSIBLE_NETWORK_GROUP_MODULES") {
            cfg.network_group_modules = Some(name_list(v));
        }
        // Note the spelling: the env var is DUPLICATE_YAML_DICT_KEY, the ini key is not.
        if let Some(v) = env.var("ANSIBLE_DUPLICATE_YAML_DICT_KEY").and_then(DuplicateDictKey::parse)
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

/// Builds an [`AnsibleConfig`] with any dependency not supplied defaulting to the real
/// one — [`StdFs`], a fresh [`EnvMap::from_process`]. The default is silent by design
/// (accepted trade-off, T-098): a test that wants isolation from the developer's shell
/// must say `.env(&EnvMap::empty())` — omitting it means the real environment applies.
pub struct AnsibleConfigBuilder<'a> {
    root: &'a Path,
    fs: Option<&'a dyn Fs>,
    env: Option<&'a EnvMap>,
}

impl<'a> AnsibleConfigBuilder<'a> {
    pub fn fs(mut self, fs: &'a dyn Fs) -> Self {
        self.fs = Some(fs);
        self
    }

    pub fn env(mut self, env: &'a EnvMap) -> Self {
        self.env = Some(env);
        self
    }

    pub fn load(self) -> AnsibleConfig {
        let fs = self.fs.unwrap_or(&StdFs);
        match self.env {
            Some(env) => AnsibleConfig::load_resolved(self.root, fs, env),
            None => AnsibleConfig::load_resolved(self.root, fs, &EnvMap::from_process()),
        }
    }
}

/// The file `ANSIBLE_CONFIG` names, when it names one that exists: `~` expanded, a
/// directory value means its `ansible.cfg`, and a set-but-missing path falls through to
/// the caller's next candidate rather than erroring — Ansible does the same, dropping to
/// CWD → `~/.ansible.cfg` → `/etc/ansible/ansible.cfg` (`manager.py:296-300`). A relative
/// value resolves against the process CWD, as it does in Ansible.
fn env_config_file(fs: &dyn Fs, env: &EnvMap) -> Option<PathBuf> {
    let raw = env.var("ANSIBLE_CONFIG")?;
    let mut p = match raw.strip_prefix("~/") {
        Some(rest) => PathBuf::from(env.var("HOME")?).join(rest),
        None => PathBuf::from(raw),
    };
    if fs.is_dir(&p) {
        p = p.join("ansible.cfg");
    }
    fs.is_file(&p).then_some(p)
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
fn expand_list(value: &str, base: &Path, env: &EnvMap) -> Vec<PathBuf> {
    value
        .split(':')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| expand_path(s, base, env))
        .collect()
}

/// One entry of an [`expand_list`], and the shape of every `type: path` value.
fn expand_path(s: &str, base: &Path, env: &EnvMap) -> PathBuf {
    match s.strip_prefix("~/") {
        Some(rest) => match env.var("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => PathBuf::from(s),
        },
        None if Path::new(s).is_absolute() => PathBuf::from(s),
        None => base.join(s.trim_start_matches("./")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::CfgFs;

    fn cfg(text: &str) -> AnsibleConfig {
        AnsibleConfig::builder(Path::new("/p")).fs(&CfgFs::some(text)).env(&EnvMap::empty()).load()
    }

    /// T-102. All three spellings, plus the two ways a value can be absent. Live-verified
    /// against ansible-core 2.21.2 — see [`DuplicateDictKey`] for what it rejects.
    #[test]
    fn duplicate_dict_key_reads_all_three_values() {
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

    /// T-072.
    #[test]
    fn network_group_modules_falls_back_then_is_replaced_by_the_cfg_key() {
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
    /// read from a private repo under `$HOME` (T-077); `HOME` comes from the injected
    /// environment, so the `~` assertion holds everywhere, Windows included.
    #[test]
    fn a_colon_list_expands_tilde_and_dot_against_the_project_root() {
        let root = crate::testing::project(
            "cfg-colon-list",
            "[defaults]\nroles_path = ~/ansible/roles:./roles\ncollections_path = ./collections\n",
            &[("roles/.keep", ""), ("collections/.keep", "")],
        );
        let cfg = AnsibleConfig::builder(&root)
            .env(&EnvMap::from_pairs(&[("HOME", "/home/t")]))
            .load();

        assert_eq!(cfg.roles_path.len(), 2, "a colon splits entries: {:?}", cfg.roles_path);
        assert_eq!(cfg.roles_path[0], PathBuf::from("/home/t/ansible/roles"));
        assert_eq!(cfg.roles_path[1], root.join("roles"), "`./` is project-root relative");
        assert!(cfg.collections_path.contains(&root.join("collections")));
    }

    /// T-072, moved from its own single-test binary when `EnvMap` made env injectable.
    #[test]
    fn the_network_env_var_overrides_both_the_cfg_key_and_the_default() {
        let env = EnvMap::from_pairs(&[("ANSIBLE_NETWORK_GROUP_MODULES", "junos, vyos")]);

        let c = AnsibleConfig::builder(Path::new("/p"))
            .fs(&CfgFs::some("[defaults]\nnetwork_group_modules = ios\n"))
            .env(&env)
            .load();
        assert!(c.is_network_platform("junos"), "env value in force");
        assert!(c.is_network_platform("vyos"));
        assert!(!c.is_network_platform("ios"), "env replaces the cfg key wholesale");

        let c = AnsibleConfig::builder(Path::new("/p")).fs(&CfgFs::none()).env(&env).load();
        assert!(c.is_network_platform("junos"), "env honoured without an ansible.cfg");
        assert!(!c.is_network_platform("ios"));
    }

    /// T-102, likewise. Live-verified against ansible-core 2.21.2 — cfg `error` + env
    /// `ignore` runs silently, cfg `ignore` + env `error` refuses to load the file, and
    /// the env var applies with no `ansible.cfg` present at all.
    #[test]
    fn the_duplicate_key_env_var_overrides_both_the_cfg_key_and_the_default() {
        use DuplicateDictKey::*;
        let with = |cfg: Option<&str>, env: &EnvMap| {
            let fs = cfg.map(CfgFs::some).unwrap_or_else(CfgFs::none);
            AnsibleConfig::builder(Path::new("/p")).fs(&fs).env(env).load().duplicate_dict_key
        };
        const ERROR_CFG: &str = "[defaults]\nduplicate_dict_key = error\n";
        const IGNORE_CFG: &str = "[defaults]\nduplicate_dict_key = ignore\n";

        let ignore = EnvMap::from_pairs(&[("ANSIBLE_DUPLICATE_YAML_DICT_KEY", "ignore")]);
        assert_eq!(with(Some(ERROR_CFG), &ignore), Ignore, "env beats the cfg key");
        assert_eq!(with(None, &ignore), Ignore, "env applies with no ansible.cfg");

        // And in the other direction, so this can't pass by preferring the quieter value.
        let error = EnvMap::from_pairs(&[("ANSIBLE_DUPLICATE_YAML_DICT_KEY", "error")]);
        assert_eq!(with(Some(IGNORE_CFG), &error), Error);

        // A value Ansible rejects leaves the layer below intact rather than aborting, which
        // is where we knowingly diverge: Ansible refuses to run at all.
        let bad = EnvMap::from_pairs(&[("ANSIBLE_DUPLICATE_YAML_DICT_KEY", "False")]);
        assert_eq!(with(Some(IGNORE_CFG), &bad), Ignore, "cfg key survives");
        assert_eq!(with(None, &bad), Warn, "and the default survives");
    }

    /// T-098. `ANSIBLE_CONFIG` picks the config file outright, beating the project walk.
    #[test]
    fn ansible_config_replaces_the_project_file_wholesale() {
        use crate::testing::MemFs;
        let fs = MemFs::new(&[
            ("/p/ansible.cfg", "[defaults]\nroles_path = ./roles\n"),
            ("/elsewhere/team.cfg", "[defaults]\nlibrary = ./mods\n"),
        ]);

        let c = AnsibleConfig::builder(Path::new("/p"))
            .fs(&fs)
            .env(&EnvMap::from_pairs(&[("ANSIBLE_CONFIG", "/elsewhere/team.cfg")]))
            .load();
        assert_eq!(
            c.library,
            vec![PathBuf::from("/elsewhere/mods")],
            "the env file's relative entries anchor to its own directory, not the project"
        );
        assert!(
            c.roles_path.is_empty(),
            "first hit wins whole: the project file is not read, let alone merged"
        );
    }

    /// A directory value means its `ansible.cfg` (`manager.py:268-270`), and a value naming
    /// nothing falls through to the project file, as Ansible drops to its CWD → HOME →
    /// /etc candidates rather than erroring.
    #[test]
    fn ansible_config_takes_a_directory_and_a_missing_path_falls_through() {
        use crate::testing::MemFs;
        let fs = MemFs::new(&[
            ("/p/ansible.cfg", "[defaults]\nroles_path = ./roles\n"),
            ("/elsewhere/d/ansible.cfg", "[defaults]\nroles_path = ./shared\n"),
        ]);
        let load = |env: &EnvMap| AnsibleConfig::builder(Path::new("/p")).fs(&fs).env(env).load();

        let c = load(&EnvMap::from_pairs(&[("ANSIBLE_CONFIG", "/elsewhere/d")]));
        assert_eq!(c.roles_path, vec![PathBuf::from("/elsewhere/d/shared")]);

        let c = load(&EnvMap::from_pairs(&[("ANSIBLE_CONFIG", "/nowhere/ansible.cfg")]));
        assert_eq!(c.roles_path, vec![PathBuf::from("/p/roles")]);

        let c = load(&EnvMap::empty());
        assert_eq!(c.roles_path, vec![PathBuf::from("/p/roles")], "unset: the walk's file");
    }

    /// T-098. `ANSIBLE_HOME` resolves env → the `home` ini key → `~/.ansible`.
    #[test]
    fn ansible_home_defaults_under_home_and_is_none_without_one() {
        let c = AnsibleConfig::builder(Path::new("/p"))
            .fs(&CfgFs::none())
            .env(&EnvMap::from_pairs(&[("HOME", "/home/t")]))
            .load();
        assert_eq!(c.ansible_home, Some(PathBuf::from("/home/t/.ansible")));

        let c =
            AnsibleConfig::builder(Path::new("/p")).fs(&CfgFs::none()).env(&EnvMap::empty()).load();
        assert_eq!(c.ansible_home, None, "no setting and no HOME: nothing to resolve");
    }

    #[test]
    fn the_home_ini_key_relocates_ansible_home() {
        let c = AnsibleConfig::builder(Path::new("/p"))
            .fs(&CfgFs::some("[defaults]\nhome = /opt/ans\n"))
            .env(&EnvMap::from_pairs(&[("HOME", "/home/t")]))
            .load();
        assert_eq!(c.ansible_home, Some(PathBuf::from("/opt/ans")), "ini beats the default");

        // A `~` value goes through the same expansion as every path key.
        let c = AnsibleConfig::builder(Path::new("/p"))
            .fs(&CfgFs::some("[defaults]\nhome = ~/ans\n"))
            .env(&EnvMap::from_pairs(&[("HOME", "/home/t")]))
            .load();
        assert_eq!(c.ansible_home, Some(PathBuf::from("/home/t/ans")));
    }

    /// T-098. `ANSIBLE_ROLES_PATH` replaces the ini's `roles_path` wholesale — no merging,
    /// same as every layer of the ladder.
    #[test]
    fn the_roles_path_env_var_beats_the_cfg_key() {
        let env = EnvMap::from_pairs(&[("ANSIBLE_ROLES_PATH", "/abs/roles:~/r"), ("HOME", "/home/t")]);

        let c = AnsibleConfig::builder(Path::new("/p"))
            .fs(&CfgFs::some("[defaults]\nroles_path = ./from_ini\n"))
            .env(&env)
            .load();
        assert_eq!(
            c.roles_path,
            vec![PathBuf::from("/abs/roles"), PathBuf::from("/home/t/r")],
            "env replaces the cfg key wholesale, with the usual `:`-split and `~` expansion"
        );

        let c = AnsibleConfig::builder(Path::new("/p")).fs(&CfgFs::none()).env(&env).load();
        assert_eq!(
            c.roles_path,
            vec![PathBuf::from("/abs/roles"), PathBuf::from("/home/t/r")],
            "env applies with no ansible.cfg at all"
        );
    }

    /// T-098. Note the spelling: the env var is singular (`base.yml:281-283`); only the
    /// ini key still accepts the legacy plural.
    #[test]
    fn the_collections_path_env_var_beats_the_cfg_key() {
        let env = EnvMap::from_pairs(&[("ANSIBLE_COLLECTIONS_PATH", "/site/coll")]);

        let c = AnsibleConfig::builder(Path::new("/p"))
            .fs(&CfgFs::some("[defaults]\ncollections_path = ./from_ini\n"))
            .env(&env)
            .load();
        assert_eq!(
            c.collections_path,
            vec![PathBuf::from("/site/coll")],
            "env replaces the cfg key wholesale"
        );

        let c = AnsibleConfig::builder(Path::new("/p")).fs(&CfgFs::none()).env(&env).load();
        assert_eq!(c.collections_path, vec![PathBuf::from("/site/coll")], "no ansible.cfg");
    }

    /// T-098.
    #[test]
    fn the_library_env_var_beats_the_cfg_key() {
        let env = EnvMap::from_pairs(&[("ANSIBLE_LIBRARY", "/site/modules")]);

        let c = AnsibleConfig::builder(Path::new("/p"))
            .fs(&CfgFs::some("[defaults]\nlibrary = ./from_ini\n"))
            .env(&env)
            .load();
        assert_eq!(
            c.library,
            vec![PathBuf::from("/site/modules")],
            "env replaces the cfg key wholesale"
        );

        let c = AnsibleConfig::builder(Path::new("/p")).fs(&CfgFs::none()).env(&env).load();
        assert_eq!(c.library, vec![PathBuf::from("/site/modules")], "no ansible.cfg");
    }

    #[test]
    fn the_ansible_home_env_var_beats_the_home_ini_key() {
        let c = AnsibleConfig::builder(Path::new("/p"))
            .fs(&CfgFs::some("[defaults]\nhome = /opt/ini\n"))
            .env(&EnvMap::from_pairs(&[("ANSIBLE_HOME", "/opt/env"), ("HOME", "/home/t")]))
            .load();
        assert_eq!(c.ansible_home, Some(PathBuf::from("/opt/env")));
    }
}
