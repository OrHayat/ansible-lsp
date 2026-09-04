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
        // `vars_os` + filter, not `vars()`: the latter panics on a non-Unicode value, and
        // a config reader must treat such a variable as unset, not kill the server.
        Self(
            std::env::vars_os()
                .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
                .collect(),
        )
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

#[derive(Debug, Clone)]
pub struct AnsibleConfig {
    /// Every path list is `None` = never set (the built-in defaults apply) vs
    /// `Some(vec![])` = explicitly emptied (`ANSIBLE_ROLES_PATH=`, a bare `roles_path =`),
    /// which *disables* the defaults — Ansible's pathlist turns "" into [], replacing the
    /// default like any other value, so the two cases must not collapse.
    pub roles_path: Option<Vec<PathBuf>>,
    pub collections_path: Option<Vec<PathBuf>>,
    /// The `library` key — `DEFAULT_MODULE_PATH`'s ini name (`config/base.yml:945-951`).
    /// When set it *replaces* the default legacy module dirs, not appends.
    pub library: Option<Vec<PathBuf>>,
    /// The `action_plugins` key — `DEFAULT_ACTION_PLUGIN_PATH`'s ini name. Legacy
    /// controller-side plugin dirs; a plugin here overrides a same-named module.
    pub action_plugins: Option<Vec<PathBuf>>,
    /// The `inventory` key — `DEFAULT_HOST_LIST` (`config/base.yml:797-808`), the inventory
    /// sources to read when no `-i` is given. Alone among these lists it is `type: pathlist`
    /// and so splits on **comma**, not `os.pathsep` (`config/manager.py:197-199`); the
    /// others are `pathspec`. Ansible's own default is `[/etc/ansible/hosts]`, applied by
    /// the reader rather than stored here, so `None` stays "never set" as elsewhere.
    pub inventory: Option<Vec<PathBuf>>,
    /// The file discovery settled on — the env-selected or walk-found `ansible.cfg` that
    /// was actually read; `None` when neither existed. The scan report prints it, and it
    /// is `{{ ansible_config_file }}`'s value (`vars/manager.py:457` — the magic var is
    /// the config in effect, undefined when there is none).
    pub config_file: Option<PathBuf>,
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
    /// `INVALID_TASK_ATTRIBUTE_FAILED` (`base.yml:1703-1713`): true — the shipped
    /// default — makes an unknown task attribute a load error, false downgrades it to
    /// `Ignoring invalid attribute`. Task-level only; plays and blocks stay fatal. T-107.
    pub invalid_task_attribute_failed: bool,
    /// `DEFAULT_JINJA2_EXTENSIONS` — ini `[defaults] jinja2_extensions`, env
    /// `ANSIBLE_JINJA2_EXTENSIONS`, `type: list`, default `[]`, deprecated as of 2.23. An
    /// extension registers tags, so with one loaded a tag outside jinja's fourteen is legal
    /// and the unknown-tag diagnostic must go quiet (T-040).
    ///
    /// Measured on ansible-core 2.21.2, both spellings, with the same template both ways:
    /// `{% for i in [1,2,3] %}{% if i == 2 %}{% break %}{% endif %}{{ i }}{% endfor %}` is
    /// `Encountered unknown tag 'break'` by default and renders `1` under
    /// `jinja2.ext.loopcontrols`.
    pub jinja2_extensions: Vec<String>,
    /// `CACHE_PLUGIN` — ini `[defaults] fact_caching`, env `ANSIBLE_CACHE_PLUGIN`, default
    /// `memory`. Anything else persists facts across runs, so a play with
    /// `gather_facts: false` still sees them (measured on 2.21.2 with `jsonfile`: a second
    /// run's facts-off play answered `ansible_os_family is defined` with `True`). `None` is
    /// "never set", which means `memory`. T-224.
    pub fact_caching: Option<String>,
}

/// Hand-written for one field: `invalid_task_attribute_failed` defaults *true*, which
/// `#[derive(Default)]` cannot express.
impl Default for AnsibleConfig {
    fn default() -> Self {
        Self {
            roles_path: None,
            collections_path: None,
            library: None,
            action_plugins: None,
            inventory: None,
            config_file: None,
            ansible_home: None,
            network_group_modules: None,
            duplicate_dict_key: DuplicateDictKey::default(),
            invalid_task_attribute_failed: true,
            jinja2_extensions: Vec::new(),
            fact_caching: None,
        }
    }
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
        // Two passes, because configparser interpolates at read time over the whole parsed
        // section — `%(key)s` may reference an entry defined later in the file (T-145).
        let mut entries = Vec::new();
        let text = fs.read(&file);
        if text.is_some() {
            cfg.config_file = Some(file.clone());
        }
        if let Some(text) = text {
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
                // Keys are case-folded (configparser's `optionxform`), and `;` after
                // whitespace starts an inline comment — `manager.py:432` enables exactly
                // that prefix and no other, so an inline `#` is part of the value.
                entries.push((
                    key.trim().to_lowercase(),
                    strip_inline_comment(value.trim()).to_string(),
                ));
            }
        }
        let section: HashMap<String, String> = entries.iter().cloned().collect();
        for (key, value) in &entries {
            let value = interpolate(value, &section);
            let paths = || expand_list(&value, base, env);
            match key.as_str() {
                "home" => home_key = Some(value.clone()),
                "roles_path" => cfg.roles_path = Some(paths()),
                "collections_path" | "collections_paths" => cfg.collections_path = Some(paths()),
                "library" => cfg.library = Some(paths()),
                "action_plugins" => cfg.action_plugins = Some(paths()),
                // `pathlist`, not `pathspec`: comma-separated (`config/manager.py:197`).
                "inventory" => cfg.inventory = Some(expand_comma_list(&value, base, env)),
                "network_group_modules" => {
                    cfg.network_group_modules = Some(name_list(&value));
                }
                "jinja2_extensions" => cfg.jinja2_extensions = name_list(&value),
                "duplicate_dict_key" => {
                    if let Some(v) = DuplicateDictKey::parse(&value) {
                        cfg.duplicate_dict_key = v;
                    }
                }
                "invalid_task_attribute_failed" => {
                    if let Some(v) = parse_bool(&value) {
                        cfg.invalid_task_attribute_failed = v;
                    }
                }
                "fact_caching" => cfg.fact_caching = Some(value.trim().to_string()),
                _ => {}
            }
        }
        // Env beats the ini file, which beats the shipped default — and it applies whether
        // or not an `ansible.cfg` was found, so it can't ride inside the block above.
        // Ansible resolves env path values against the invocation CWD; an editor has no
        // meaningful one, so the project root stands in — NOT `base`, which may be an
        // env-selected config's unrelated directory.
        for (var, slot) in [
            ("ANSIBLE_ROLES_PATH", &mut cfg.roles_path),
            ("ANSIBLE_COLLECTIONS_PATH", &mut cfg.collections_path),
            ("ANSIBLE_LIBRARY", &mut cfg.library),
            ("ANSIBLE_ACTION_PLUGINS", &mut cfg.action_plugins),
        ] {
            if let Some(v) = env.var(var) {
                *slot = Some(expand_list(v, project_root, env));
            }
        }
        // Separate from the loop above: `ANSIBLE_INVENTORY` is the one pathlist here, so it
        // splits on comma while every entry above splits on `os.pathsep`.
        if let Some(v) = env.var("ANSIBLE_INVENTORY") {
            cfg.inventory = Some(expand_comma_list(v, project_root, env));
        }
        cfg.ansible_home = env
            .var("ANSIBLE_HOME")
            .map(|v| expand_path(v, base, env))
            .or_else(|| home_key.map(|v| expand_path(&v, base, env)))
            .or_else(|| env.var("HOME").map(|h| PathBuf::from(h).join(".ansible")));
        if let Some(v) = env.var("ANSIBLE_NETWORK_GROUP_MODULES") {
            cfg.network_group_modules = Some(name_list(v));
        }
        if let Some(v) = env.var("ANSIBLE_JINJA2_EXTENSIONS") {
            cfg.jinja2_extensions = name_list(v);
        }
        // Note the spelling: the env var is DUPLICATE_YAML_DICT_KEY, the ini key is not.
        if let Some(v) = env.var("ANSIBLE_DUPLICATE_YAML_DICT_KEY").and_then(DuplicateDictKey::parse)
        {
            cfg.duplicate_dict_key = v;
        }
        if let Some(v) = env.var("ANSIBLE_INVALID_TASK_ATTRIBUTE_FAILED").and_then(parse_bool) {
            cfg.invalid_task_attribute_failed = v;
        }
        if let Some(v) = env.var("ANSIBLE_CACHE_PLUGIN") {
            cfg.fact_caching = Some(v.trim().to_string());
        }
        cfg
    }

    /// Whether facts can outlive the run that gathered them: any cache plugin but the
    /// default `memory` one.
    pub fn facts_persist(&self) -> bool {
        self.fact_caching.as_deref().is_some_and(|p| p != "memory")
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
    // Ansible unfrackpaths the value against the invocation CWD (`manager.py:267`); the
    // server's own CWD is the nearest thing it has, and keeping the path relative would
    // poison `base` and every list expanded against it. `~user` (pwd-database) forms and
    // a HOME-less `~` stay literal and fall through to the project file — a documented
    // gap, not a crash.
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut p = expand_path(raw, &cwd, env);
    if p.is_relative() {
        p = cwd.join(p);
    }
    if fs.is_dir(&p) {
        p = p.join("ansible.cfg");
    }
    fs.is_file(&p).then_some(p)
}

/// Python configparser's `BasicInterpolation`, on for `ansible.cfg` (`manager.py:432`):
/// `%(key)s` references another `[defaults]` value — case-insensitive, forward references
/// allowed — and `%%` is a literal `%`. Where configparser aborts every ansible command
/// (unknown key, bare `%`, a cycle past its depth cap of 10) the raw value stands instead,
/// the [`DuplicateDictKey::parse`] divergence: going dark over one config line is worse.
/// All verified live against Python 3 configparser (T-145).
fn interpolate(value: &str, section: &HashMap<String, String>) -> String {
    fn go(value: &str, section: &HashMap<String, String>, depth: u8) -> Option<String> {
        if depth == 0 {
            return None;
        }
        let mut out = String::with_capacity(value.len());
        let mut rest = value;
        while let Some(i) = rest.find('%') {
            out.push_str(&rest[..i]);
            rest = &rest[i + 1..];
            if let Some(t) = rest.strip_prefix('%') {
                out.push('%');
                rest = t;
            } else if let Some(t) = rest.strip_prefix('(') {
                let end = t.find(')')?;
                let referenced = section.get(&t[..end].to_lowercase())?;
                out.push_str(&go(referenced, section, depth - 1)?);
                rest = t[end + 1..].strip_prefix('s')?;
            } else {
                return None;
            }
        }
        out.push_str(rest);
        Some(out)
    }
    go(value, section, 10).unwrap_or_else(|| value.to_string())
}

/// `;` preceded by whitespace cuts the value; nothing else does — not `#`, not a `;` glued
/// to the value (both verified against configparser with `manager.py:432`'s prefixes).
fn strip_inline_comment(value: &str) -> &str {
    let mut prev_ws = false;
    for (i, ch) in value.char_indices() {
        if ch == ';' && prev_ws {
            return value[..i].trim_end();
        }
        prev_ws = ch.is_whitespace();
    }
    value
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

/// A boolean the way Ansible's `boolean()` reads one (`convert_bool.py:12-13`):
/// `y yes on 1 true t` / `n no off 0 false f`, case-insensitive. `None` for any other
/// spelling — callers keep the shipped default rather than refusing to serve.
fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_lowercase().as_str() {
        "y" | "yes" | "on" | "1" | "true" | "t" => Some(true),
        "n" | "no" | "off" | "0" | "false" | "f" => Some(false),
        _ => None,
    }
}

/// Colon-separated list; `~` expanded, relative entries resolved against the config's
/// own directory (Ansible resolves them against cwd, which is where you run it from).
/// A `type: pathlist` value — comma-separated, whitespace stripped
/// (`config/manager.py:197-199`). Only `inventory` is one; see [`expand_list`] for the
/// `pathspec` majority, which splits on `os.pathsep` instead.
fn expand_comma_list(value: &str, base: &Path, env: &EnvMap) -> Vec<PathBuf> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| expand_path(s, base, env))
        .collect()
}

fn expand_list(value: &str, base: &Path, env: &EnvMap) -> Vec<PathBuf> {
    value
        .split(':')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| expand_path(s, base, env))
        .collect()
}

/// One entry of an [`expand_list`], and the shape of every `type: path` value. A `~` that
/// can't expand (no `HOME`, or the pwd-database `~user` form) stays literal rather than
/// anchoring to `base` — a wrong-but-absolute guess would be worse than a miss.
pub(crate) fn expand_path(s: &str, base: &Path, env: &EnvMap) -> PathBuf {
    if s == "~" {
        if let Some(home) = env.var("HOME") {
            return PathBuf::from(home);
        }
    }
    match s.strip_prefix("~/") {
        Some(rest) => match env.var("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => PathBuf::from(s),
        },
        None if Path::new(s).is_absolute() || s.starts_with('~') => PathBuf::from(s),
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

    /// T-107. Shipped default true; the ini key and env var flip it; env beats ini;
    /// Ansible's full boolean vocabulary is accepted, and a bad spelling keeps the
    /// default rather than going dark.
    #[test]
    fn invalid_task_attribute_failed_reads_ini_and_env() {
        assert!(cfg("[defaults]\nroles_path = ./roles\n").invalid_task_attribute_failed);
        assert!(!cfg("[defaults]\ninvalid_task_attribute_failed = False\n").invalid_task_attribute_failed);
        assert!(!cfg("[defaults]\ninvalid_task_attribute_failed = no\n").invalid_task_attribute_failed);
        assert!(cfg("[defaults]\ninvalid_task_attribute_failed = on\n").invalid_task_attribute_failed);
        assert!(cfg("[defaults]\ninvalid_task_attribute_failed = maybe\n").invalid_task_attribute_failed);

        let env = EnvMap::from_pairs(&[("ANSIBLE_INVALID_TASK_ATTRIBUTE_FAILED", "0")]);
        let got = AnsibleConfig::builder(Path::new("/p"))
            .fs(&CfgFs::some("[defaults]\ninvalid_task_attribute_failed = true\n"))
            .env(&env)
            .load();
        assert!(!got.invalid_task_attribute_failed, "env must beat the ini value");
    }

    /// T-224. Unset is `memory`; any other plugin persists facts; env beats ini.
    #[test]
    fn fact_caching_reads_ini_and_env() {
        assert!(!cfg("[defaults]\nroles_path = ./roles\n").facts_persist());
        assert!(!cfg("[defaults]\nfact_caching = memory\n").facts_persist());
        assert!(cfg("[defaults]\nfact_caching = jsonfile\n").facts_persist());
        assert!(cfg("[defaults]\nfact_caching = redis \n").facts_persist());
        let got = AnsibleConfig::builder(Path::new("/p"))
            .fs(&CfgFs::some("[defaults]\nfact_caching = jsonfile\n"))
            .env(&EnvMap::from_pairs(&[("ANSIBLE_CACHE_PLUGIN", "memory")]))
            .load();
        assert!(!got.facts_persist(), "env must beat the ini value");
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

    /// T-062: `inventory` is `DEFAULT_HOST_LIST`, the only `type: pathlist` among these
    /// keys, so it splits on **comma** while every neighbour splits on `os.pathsep`
    /// (`config/manager.py:190-201`). Reusing the colon splitter would silently read
    /// `a.yml,b.yml` as one path named `a.yml,b.yml` and index nothing.
    #[test]
    fn inventory_is_a_comma_list_while_its_neighbours_are_colon_lists() {
        let c = cfg("[defaults]\ninventory = a.yml,b.yml\nroles_path = r1:r2\n");
        let inv = c.inventory.expect("set by the cfg");
        assert_eq!(inv.len(), 2, "comma-separated: {inv:?}");
        assert!(inv[0].ends_with("a.yml") && inv[1].ends_with("b.yml"), "{inv:?}");
        // The control, one line up in the same file: colons still split colons.
        assert_eq!(c.roles_path.expect("set").len(), 2);
        // A colon in an inventory value is a filename character, not a separator.
        let c = cfg("[defaults]\ninventory = a.yml:b.yml\n");
        assert_eq!(c.inventory.expect("set").len(), 1);
        // Whitespace around entries is stripped, as `pathlist` does.
        let c = cfg("[defaults]\ninventory = a.yml , b.yml\n");
        assert_eq!(c.inventory.expect("set").len(), 2);
        // Never set stays None, so the reader can apply /etc/ansible/hosts itself; an
        // explicit empty is a deliberate "no inventory" and must not collapse into it.
        assert!(cfg("[defaults]\nroles_path = r\n").inventory.is_none());
        assert_eq!(cfg("[defaults]\ninventory =\n").inventory, Some(Vec::new()));
    }

    /// The env var is the same pathlist type, and beats the file — measured on 2.21.2,
    /// where `ANSIBLE_INVENTORY` overrode `ansible.cfg` outright.
    #[test]
    fn ansible_inventory_env_is_a_comma_list_and_beats_the_file() {
        let c = AnsibleConfig::builder(Path::new("/p"))
            .fs(&CfgFs::some("[defaults]\ninventory = from_cfg.yml\n"))
            .env(&EnvMap::from_pairs(&[("ANSIBLE_INVENTORY", "env_a.yml,env_b.yml")]))
            .load();
        let inv = c.inventory.expect("set by the env");
        assert_eq!(inv.len(), 2, "{inv:?}");
        assert!(inv[0].ends_with("env_a.yml"), "env replaces the file: {inv:?}");
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

        let roles = cfg.roles_path.expect("set by the cfg");
        assert_eq!(roles.len(), 2, "a colon splits entries: {roles:?}");
        assert_eq!(roles[0], PathBuf::from("/home/t/ansible/roles"));
        assert_eq!(roles[1], root.join("roles"), "`./` is project-root relative");
        assert!(cfg.collections_path.expect("set").contains(&root.join("collections")));
    }

    /// `DEFAULT_JINJA2_EXTENSIONS` — the key that decides whether T-040's template
    /// diagnostic is allowed to speak at all, so both spellings are read and the env one
    /// wins. Live-verified on ansible-core 2.21.2 with `ansible-config dump`:
    /// `DEFAULT_JINJA2_EXTENSIONS(/tmp/extprobe/ansible.cfg) = ['jinja2.ext.loopcontrols']`.
    #[test]
    fn the_jinja_extensions_key_is_read_from_both_the_cfg_and_the_env() {
        // The default is `[]`, which is what lets the diagnostic fire at all.
        let bare = AnsibleConfig::builder(Path::new("/p")).fs(&CfgFs::none()).load();
        assert!(bare.jinja2_extensions.is_empty());

        let ini = AnsibleConfig::builder(Path::new("/p"))
            .fs(&CfgFs::some("[defaults]
jinja2_extensions = jinja2.ext.loopcontrols
"))
            .load();
        assert_eq!(ini.jinja2_extensions, ["jinja2.ext.loopcontrols"]);

        // A list, comma-separated and trimmed, and the env var replaces the file's value.
        let both = AnsibleConfig::builder(Path::new("/p"))
            .fs(&CfgFs::some("[defaults]
jinja2_extensions = jinja2.ext.debug
"))
            .env(&EnvMap::from_pairs(&[(
                "ANSIBLE_JINJA2_EXTENSIONS",
                "jinja2.ext.i18n, jinja2.ext.do",
            )]))
            .load();
        assert_eq!(both.jinja2_extensions, ["jinja2.ext.i18n", "jinja2.ext.do"]);
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

    /// A fixture path that is absolute on the *host*.
    ///
    /// The fixtures are written POSIX-style, but on Windows a rooted path with no drive
    /// prefix is **relative** — `Path::is_absolute` is false — so an `ANSIBLE_CONFIG` value
    /// like `/elsewhere/team.cfg` takes the `is_relative` branch in [`env_config_file`], gets
    /// re-anchored onto the cwd's drive, and misses the `MemFs` key. Only the paths that must
    /// survive that branch need this; everything else compares by components, where `/` and
    /// `\` are the same separator, so the rest of the fixtures stay as written.
    fn abs(p: &str) -> String {
        if cfg!(windows) {
            format!("C:{p}")
        } else {
            p.to_string()
        }
    }

    /// T-098. `ANSIBLE_CONFIG` picks the config file outright, beating the project walk.
    #[test]
    fn ansible_config_replaces_the_project_file_wholesale() {
        use crate::testing::MemFs;
        let team = abs("/elsewhere/team.cfg");
        let fs = MemFs::new(&[
            ("/p/ansible.cfg", "[defaults]\nroles_path = ./roles\n"),
            (&team, "[defaults]\nlibrary = ./mods\n"),
        ]);

        let c = AnsibleConfig::builder(Path::new("/p"))
            .fs(&fs)
            .env(&EnvMap::from_pairs(&[("ANSIBLE_CONFIG", &team)]))
            .load();
        assert_eq!(
            c.library,
            Some(vec![PathBuf::from(abs("/elsewhere/mods"))]),
            "the env file's relative entries anchor to its own directory, not the project"
        );
        assert!(
            c.roles_path.is_none(),
            "first hit wins whole: the project file is not read, let alone merged"
        );
    }

    /// A directory value means its `ansible.cfg` (`manager.py:268-270`), and a value naming
    /// nothing falls through to the project file, as Ansible drops to its CWD → HOME →
    /// /etc candidates rather than erroring.
    #[test]
    fn ansible_config_takes_a_directory_and_a_missing_path_falls_through() {
        use crate::testing::MemFs;
        let dir_cfg = abs("/elsewhere/d/ansible.cfg");
        let fs = MemFs::new(&[
            ("/p/ansible.cfg", "[defaults]\nroles_path = ./roles\n"),
            (&dir_cfg, "[defaults]\nroles_path = ./shared\n"),
        ]);
        let load = |env: &EnvMap| AnsibleConfig::builder(Path::new("/p")).fs(&fs).env(env).load();

        let c = load(&EnvMap::from_pairs(&[("ANSIBLE_CONFIG", &abs("/elsewhere/d"))]));
        assert_eq!(c.roles_path, Some(vec![PathBuf::from(abs("/elsewhere/d/shared"))]));

        let c = load(&EnvMap::from_pairs(&[("ANSIBLE_CONFIG", &abs("/nowhere/ansible.cfg"))]));
        assert_eq!(c.roles_path, Some(vec![PathBuf::from("/p/roles")]));

        let c = load(&EnvMap::empty());
        assert_eq!(c.roles_path, Some(vec![PathBuf::from("/p/roles")]), "unset: the walk's file");
    }

    /// T-145. Verified against Python 3 configparser, the parser `manager.py:432` builds:
    /// forward and case-insensitive `%(key)s` references expand, `%%` unescapes, and keys
    /// themselves are case-folded.
    #[test]
    fn percent_interpolation_expands_references() {
        let c = cfg("[defaults]\nroles_path = %(base)s/roles:%(BASE)s/extra\nbase = /opt\n");
        assert_eq!(
            c.roles_path,
            Some(vec![PathBuf::from("/opt/roles"), PathBuf::from("/opt/extra")]),
            "forward + case-insensitive reference: configparser resolves after the parse"
        );

        let c = cfg("[defaults]\nlibrary = /x/50%%pct\n");
        assert_eq!(c.library, Some(vec![PathBuf::from("/x/50%pct")]), "%% is a literal %");

        let c = cfg("[defaults]\nRoles_Path = ./r\n");
        assert_eq!(c.roles_path, Some(vec![PathBuf::from("/p/r")]), "keys are case-folded");
    }

    /// T-145. Where configparser aborts every ansible command — unknown reference, bare
    /// `%`, reference cycle — the raw value stands and we keep serving: the deliberate
    /// [`DuplicateDictKey`] divergence. (A rule flagging the fatal line is future work.)
    #[test]
    fn broken_interpolation_keeps_the_raw_value() {
        let c = cfg("[defaults]\nlibrary = /x/%(missing)s\n");
        assert_eq!(
            c.library,
            Some(vec![PathBuf::from("/x/%(missing)s")]),
            "unknown key: InterpolationMissingOptionError in Ansible"
        );

        let c = cfg("[defaults]\nlibrary = /x/50%\n");
        assert_eq!(
            c.library,
            Some(vec![PathBuf::from("/x/50%")]),
            "bare %: InterpolationSyntaxError in Ansible"
        );

        let c = cfg("[defaults]\nlibrary = %(a)s\na = %(library)s\n");
        assert_eq!(
            c.library,
            Some(vec![PathBuf::from("/p/%(a)s")]),
            "cycle: InterpolationDepthError in Ansible; raw value, base-anchored"
        );
    }

    /// T-145. `manager.py:432` sets `inline_comment_prefixes=(';',)`: a `;` after
    /// whitespace cuts the value; a glued `;` and an inline `#` do not.
    #[test]
    fn an_inline_semicolon_comment_is_stripped_from_the_value() {
        let c = cfg("[defaults]\nroles_path = ./roles ; team convention\n");
        assert_eq!(c.roles_path, Some(vec![PathBuf::from("/p/roles")]));

        let c = cfg("[defaults]\nlibrary = /a;b\n");
        assert_eq!(c.library, Some(vec![PathBuf::from("/a;b")]), "no preceding whitespace");

        let c = cfg("[defaults]\nlibrary = /a #x\n");
        assert_eq!(c.library, Some(vec![PathBuf::from("/a #x")]), "# is not an inline prefix");
    }

    /// T-098. The file discovery settled on is recorded — the scan report and
    /// `{{ ansible_config_file }}` both need to know it, including that there was none.
    #[test]
    fn the_discovered_config_file_is_recorded() {
        use crate::testing::MemFs;
        let team = abs("/elsewhere/team.cfg");
        let fs = MemFs::new(&[("/p/ansible.cfg", "[defaults]\n"), (&team, "[defaults]\n")]);

        let c = AnsibleConfig::builder(Path::new("/p")).fs(&fs).env(&EnvMap::empty()).load();
        assert_eq!(c.config_file, Some(PathBuf::from("/p/ansible.cfg")));

        let c = AnsibleConfig::builder(Path::new("/p"))
            .fs(&fs)
            .env(&EnvMap::from_pairs(&[("ANSIBLE_CONFIG", &team)]))
            .load();
        assert_eq!(c.config_file, Some(PathBuf::from(&team)));

        let c =
            AnsibleConfig::builder(Path::new("/q")).fs(&fs).env(&EnvMap::empty()).load();
        assert_eq!(c.config_file, None, "no config anywhere: recorded as such");
    }

    /// Set-but-empty is not unset: `ANSIBLE_ROLES_PATH=` deliberately disables the
    /// built-in default dirs, exactly as an empty value does in Ansible's pathlist.
    #[test]
    fn an_empty_path_env_var_is_set_not_unset() {
        let c = AnsibleConfig::builder(Path::new("/p"))
            .fs(&CfgFs::none())
            .env(&EnvMap::from_pairs(&[("ANSIBLE_ROLES_PATH", "")]))
            .load();
        assert_eq!(c.roles_path, Some(Vec::new()), "explicitly emptied");
        assert!(c.library.is_none(), "the untouched lists stay unset");
    }

    /// Env path values anchor to the project root (our stand-in for Ansible's CWD) even
    /// when `ANSIBLE_CONFIG` moved the config elsewhere — never to the config file's dir.
    #[test]
    fn env_path_values_anchor_to_the_project_not_the_env_config() {
        use crate::testing::MemFs;
        let team = abs("/shared/team.cfg");
        let fs = MemFs::new(&[("/p/ansible.cfg", ""), (&team, "[defaults]\nlibrary = ./mods\n")]);
        let c = AnsibleConfig::builder(Path::new("/p"))
            .fs(&fs)
            .env(&EnvMap::from_pairs(&[
                ("ANSIBLE_CONFIG", &team),
                ("ANSIBLE_ROLES_PATH", "./roles"),
            ]))
            .load();
        assert_eq!(
            c.roles_path,
            Some(vec![PathBuf::from("/p/roles")]),
            "the env value means the project's roles, not /shared/roles"
        );
        assert_eq!(
            c.library,
            Some(vec![PathBuf::from(abs("/shared/mods"))]),
            "the ini's own entries keep anchoring to their config file"
        );
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

        let ini = CfgFs::some("[defaults]\nroles_path = ./from_ini\n");
        let c = AnsibleConfig::builder(Path::new("/p")).fs(&ini).env(&EnvMap::empty()).load();
        assert_eq!(
            c.roles_path,
            Some(vec![PathBuf::from("/p/from_ini")]),
            "baseline: the ini key is in force — without this, `env wins` could pass with the\
             \n ini layer silently broken"
        );

        let c = AnsibleConfig::builder(Path::new("/p")).fs(&ini).env(&env).load();
        assert_eq!(
            c.roles_path,
            Some(vec![PathBuf::from("/abs/roles"), PathBuf::from("/home/t/r")]),
            "env replaces the cfg key wholesale, with the usual `:`-split and `~` expansion"
        );

        let c = AnsibleConfig::builder(Path::new("/p")).fs(&CfgFs::none()).env(&env).load();
        assert_eq!(
            c.roles_path,
            Some(vec![PathBuf::from("/abs/roles"), PathBuf::from("/home/t/r")]),
            "env applies with no ansible.cfg at all"
        );
    }

    /// T-098. Note the spelling: the env var is singular (`base.yml:281-283`); only the
    /// ini key still accepts the legacy plural.
    #[test]
    fn the_collections_path_env_var_beats_the_cfg_key() {
        let env = EnvMap::from_pairs(&[("ANSIBLE_COLLECTIONS_PATH", "/site/coll")]);

        let ini = CfgFs::some("[defaults]\ncollections_path = ./from_ini\n");
        let c = AnsibleConfig::builder(Path::new("/p")).fs(&ini).env(&EnvMap::empty()).load();
        assert_eq!(
            c.collections_path,
            Some(vec![PathBuf::from("/p/from_ini")]),
            "baseline: the ini key is in force — without this, `env wins` could pass with the\
             \n ini layer silently broken"
        );

        let c = AnsibleConfig::builder(Path::new("/p")).fs(&ini).env(&env).load();
        assert_eq!(
            c.collections_path,
            Some(vec![PathBuf::from("/site/coll")]),
            "env replaces the cfg key wholesale"
        );

        let c = AnsibleConfig::builder(Path::new("/p")).fs(&CfgFs::none()).env(&env).load();
        assert_eq!(c.collections_path, Some(vec![PathBuf::from("/site/coll")]), "no ansible.cfg");
    }

    /// T-098.
    #[test]
    fn the_library_env_var_beats_the_cfg_key() {
        let env = EnvMap::from_pairs(&[("ANSIBLE_LIBRARY", "/site/modules")]);

        let ini = CfgFs::some("[defaults]\nlibrary = ./from_ini\n");
        let c = AnsibleConfig::builder(Path::new("/p")).fs(&ini).env(&EnvMap::empty()).load();
        assert_eq!(
            c.library,
            Some(vec![PathBuf::from("/p/from_ini")]),
            "baseline: the ini key is in force — without this, `env wins` could pass with the\
             \n ini layer silently broken"
        );

        let c = AnsibleConfig::builder(Path::new("/p")).fs(&ini).env(&env).load();
        assert_eq!(
            c.library,
            Some(vec![PathBuf::from("/site/modules")]),
            "env replaces the cfg key wholesale"
        );

        let c = AnsibleConfig::builder(Path::new("/p")).fs(&CfgFs::none()).env(&env).load();
        assert_eq!(c.library, Some(vec![PathBuf::from("/site/modules")]), "no ansible.cfg");
    }

    /// T-098. The last of the four path overrides — with it, every ini key `load_resolved`
    /// reads has its env var honoured.
    #[test]
    fn the_action_plugins_env_var_beats_the_cfg_key() {
        let env = EnvMap::from_pairs(&[("ANSIBLE_ACTION_PLUGINS", "/site/action")]);

        let ini = CfgFs::some("[defaults]\naction_plugins = ./from_ini\n");
        let c = AnsibleConfig::builder(Path::new("/p")).fs(&ini).env(&EnvMap::empty()).load();
        assert_eq!(
            c.action_plugins,
            Some(vec![PathBuf::from("/p/from_ini")]),
            "baseline: the ini key is in force — without this, `env wins` could pass with the\
             \n ini layer silently broken"
        );

        let c = AnsibleConfig::builder(Path::new("/p")).fs(&ini).env(&env).load();
        assert_eq!(
            c.action_plugins,
            Some(vec![PathBuf::from("/site/action")]),
            "env replaces the cfg key wholesale"
        );

        let c = AnsibleConfig::builder(Path::new("/p")).fs(&CfgFs::none()).env(&env).load();
        assert_eq!(c.action_plugins, Some(vec![PathBuf::from("/site/action")]), "no ansible.cfg");
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
