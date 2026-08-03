//! The installed Ansible itself — so modules and collections you didn't write are
//! still navigable, the way pyright indexes site-packages.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, Default)]
pub struct AnsibleInstall {
    /// `.../site-packages/ansible` — holds `modules/*.py` for `ansible.builtin.*`.
    pub package_dir: Option<PathBuf>,
    /// Every `ansible_collections` root outside the workspace.
    pub collection_roots: Vec<PathBuf>,
}

static DETECTED: OnceLock<AnsibleInstall> = OnceLock::new();
/// Explicit `ansible` package dir from the client's `ansibleLsp.ansiblePath` setting, seeded
/// before the first `detect()`. Config-driven, so it's per-project and live on reload.
static OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Seed the package-dir override from config. Must run before the first `detect()`; a later
/// call is ignored, since detection is cached for the process.
pub fn set_package_dir_override(dir: PathBuf) {
    let _ = OVERRIDE.set(dir);
}

impl AnsibleInstall {
    /// Detected once per process by shelling out to `ansible --version` (~300 ms).
    pub fn detect() -> &'static Self {
        DETECTED.get_or_init(Self::run)
    }

    fn run() -> Self {
        // Fast path: derive everything from the filesystem. `ansible --version` is
        // authoritative but costs ~500 ms of Python startup — and on Windows it crashes
        // outright (Ansible's control node isn't supported there), so once the filesystem
        // has found the package we must NOT fall through to it.
        let fast = Self::from_filesystem();
        if fast.package_dir.is_some() {
            return fast;
        }
        Self::from_version_command().unwrap_or(fast)
    }

    /// Locate the ansible package by resolving the `ansible` executable, plus the
    /// standard collection directories. No subprocess.
    fn from_filesystem() -> Self {
        let mut install = Self::default();

        // Explicit override from the `ansibleLsp.ansiblePath` setting: point straight at the
        // `ansible` package dir. The escape hatch for installs the walk-up can't find — uv/pipx
        // (the exe is a shim outside the venv), and Windows, where `ansible --version` crashes
        // so there's no fallback.
        if let Some(pkg) = OVERRIDE.get().cloned() {
            if pkg.join("modules").is_dir() {
                let bundled = pkg.with_file_name("ansible_collections");
                if bundled.is_dir() {
                    install.collection_roots.push(bundled);
                }
                install.package_dir = Some(pkg);
            }
        }

        if install.package_dir.is_none() {
            if let Some(exe) = which("ansible") {
                // .../bin/ansible -> .../lib/python3.x/site-packages/ansible (Unix), or
                // ...\Scripts\ansible.exe -> ...\Lib\site-packages\ansible (Windows).
                if let Some(bin) = exe.parent() {
                    if let Some(prefix) = bin.parent() {
                        if let Some(pkg) = find_site_packages(prefix) {
                            let bundled = pkg.with_file_name("ansible_collections");
                            if bundled.is_dir() {
                                install.collection_roots.push(bundled);
                            }
                            install.package_dir = Some(pkg);
                        }
                    }
                }
            }
        }

        // uv/pipx put the `ansible` executable behind a shim outside the venv, so the
        // walk-up above can't reach the package. Probe their conventional tool-install dirs
        // directly — this is what makes a `uv tool install ansible-core` just work, with no
        // env var and no working `ansible` CLI (which crashes on Windows anyway).
        if install.package_dir.is_none() {
            if let Some(pkg) = find_tool_install() {
                let bundled = pkg.with_file_name("ansible_collections");
                if bundled.is_dir() {
                    install.collection_roots.push(bundled);
                }
                install.package_dir = Some(pkg);
            }
        }

        let mut roots = Vec::new();
        if let Some(env) = std::env::var_os("ANSIBLE_COLLECTIONS_PATH") {
            roots.extend(std::env::split_paths(&env));
        }
        if let Ok(home) = std::env::var("HOME") {
            roots.push(PathBuf::from(home).join(".ansible/collections"));
        }
        roots.push(PathBuf::from("/usr/share/ansible/collections"));
        for r in roots {
            let root = r.join("ansible_collections");
            if root.is_dir() && !install.collection_roots.contains(&root) {
                install.collection_roots.push(root);
            }
        }
        install
    }

    fn from_version_command() -> Option<Self> {
        let out = std::process::Command::new("ansible")
            .arg("--version")
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut install = Self::default();

        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "ansible python module location" => {
                    let pkg = PathBuf::from(value);
                    // Collections bundled with the `ansible` distribution sit beside
                    // the package, not inside it.
                    if let Some(parent) = pkg.parent() {
                        let bundled = parent.join("ansible_collections");
                        if bundled.is_dir() {
                            install.collection_roots.push(bundled);
                        }
                    }
                    install.package_dir = Some(pkg);
                }
                "ansible collection location" => {
                    for p in value.split(':').filter(|s| !s.is_empty()) {
                        let root = PathBuf::from(p).join("ansible_collections");
                        if root.is_dir() && !install.collection_roots.contains(&root) {
                            install.collection_roots.push(root);
                        }
                    }
                }
                _ => {}
            }
        }
        Some(install)
    }

    /// Source file for a builtin module, e.g. `systemd` -> `.../modules/systemd.py`.
    pub fn builtin_module(&self, name: &str) -> Option<PathBuf> {
        let p = self.package_dir.as_ref()?.join("modules").join(format!("{name}.py"));
        p.is_file().then_some(p)
    }

    /// The 2.10 collection-split table: `plugin_routing.modules.<bare name>.redirect` from
    /// core's `config/ansible_builtin_runtime.yml` — how `docker:` resolves to
    /// `community.docker.docker` with no file anywhere in core. Consulted as the loader's
    /// last-ditch step (`loader.py:956-959`) after every path is searched.
    /// Deprecations and tombstones stay T-064.
    pub fn builtin_module_redirect(&self, name: &str) -> Option<String> {
        let pkg = self.package_dir.as_ref()?;
        module_redirect(&pkg.join("config/ansible_builtin_runtime.yml"), name)
    }
}

/// `plugin_routing.modules.<name>.redirect` from a routing table — core's or a
/// collection's `meta/runtime.yml`, both the same shape. Each file is parsed once per
/// process and cached, keyed by path.
pub fn module_redirect(table: &Path, name: &str) -> Option<String> {
    use std::collections::HashMap;
    static TABLES: OnceLock<std::sync::Mutex<HashMap<PathBuf, HashMap<String, String>>>> =
        OnceLock::new();
    let tables = TABLES.get_or_init(Default::default);
    let mut tables = tables.lock().ok()?;
    if !tables.contains_key(table) {
        let mut map = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(table) {
            let doc = crate::parse::Document::new(text);
            let modules = doc.parse().and_then(|nodes| {
                nodes.first().and_then(|n| {
                    n.get("plugin_routing")
                        .and_then(|n| n.get("modules"))
                        .cloned()
                })
            });
            if let Some(modules) = modules {
                for (k, v) in modules.entries() {
                    if let (Some(name), Some(to)) =
                        (k.as_str(), v.get("redirect").and_then(|r| r.as_str()))
                    {
                        map.insert(name.to_string(), to.to_string());
                    }
                }
            }
        }
        tables.insert(table.to_path_buf(), map);
    }
    tables.get(table)?.get(name).cloned()
}

/// First `name` on PATH, with symlinks resolved.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    // On Windows the executable is `ansible.exe` (pip console script), PATH is `;`-separated,
    // and entries contain `:` (`C:\...`). `split_paths` handles the separator per-platform;
    // the extension list covers the console-script forms.
    let exts: &[&str] = if cfg!(windows) {
        &["exe", "cmd", "bat", ""]
    } else {
        &[""]
    };
    for dir in std::env::split_paths(&path) {
        for ext in exts {
            let cand = if ext.is_empty() {
                dir.join(name)
            } else {
                dir.join(format!("{name}.{ext}"))
            };
            if cand.is_file() {
                return std::fs::canonicalize(cand).ok();
            }
        }
    }
    None
}

/// `<prefix>/lib/python3.X/site-packages/ansible`, whichever python version.
/// Find an `ansible` package inside a uv or pipx tool install, in their standard locations.
/// The tool venv is `<base>/<tool>/`, and `find_site_packages` handles the per-OS layout.
fn find_tool_install() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut bases: Vec<PathBuf> = Vec::new();
    // uv tool dir: $UV_TOOL_DIR, else %APPDATA%\uv\tools (Windows) / ~/.local/share/uv/tools.
    if let Some(d) = env("UV_TOOL_DIR") {
        bases.push(d);
    }
    if let Some(d) = env("APPDATA") {
        bases.push(d.join("uv").join("tools"));
    }
    // pipx: $PIPX_HOME/venvs, else %LOCALAPPDATA%\pipx\venvs (Windows).
    if let Some(d) = env("PIPX_HOME") {
        bases.push(d.join("venvs"));
    }
    if let Some(d) = env("LOCALAPPDATA") {
        bases.push(d.join("pipx").join("venvs"));
    }
    if let Some(h) = env("HOME") {
        bases.push(h.join(".local/share/uv/tools"));
        bases.push(h.join(".local/share/pipx/venvs"));
        bases.push(h.join(".local/pipx/venvs"));
    }
    for base in bases {
        // `uv tool install ansible-core` -> ansible-core; `pipx install ansible` -> ansible.
        for tool in ["ansible-core", "ansible"] {
            if let Some(pkg) = find_site_packages(&base.join(tool)) {
                return Some(pkg);
            }
        }
    }
    None
}

fn find_site_packages(prefix: &Path) -> Option<PathBuf> {
    let has_modules = |p: &Path| p.join("modules").is_dir();
    // `lib` and `Lib` (Windows) — separate entries matter on case-sensitive filesystems.
    for lib in ["lib", "Lib"] {
        let libdir = prefix.join(lib);
        // Windows venv: <prefix>/Lib/site-packages/ansible, no pythonX.Y level.
        let direct = libdir.join("site-packages/ansible");
        if has_modules(&direct) {
            return Some(direct);
        }
        // Unix venv/system: <prefix>/lib/pythonX.Y/site-packages/ansible.
        if let Ok(rd) = std::fs::read_dir(&libdir) {
            for entry in rd.flatten() {
                let candidate = entry.path().join("site-packages/ansible");
                if has_modules(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_local_ansible_install() {
        let i = AnsibleInstall::detect();
        if i.package_dir.is_none() {
            return; // ansible not on PATH
        }
        assert!(i.package_dir.as_ref().unwrap().ends_with("ansible"));
        assert!(
            i.builtin_module("systemd").is_some(),
            "ansible.builtin.systemd should have a source file"
        );
        assert!(!i.collection_roots.is_empty());
    }

    /// The Windows bug that hid builtins: PATH is `;`-separated there and entries hold `:`,
    /// and the exe is `<name>.exe`. Find a binary every platform ships to prove the lookup
    /// works — `cmd` on Windows (always in System32 on PATH), `sh` on Unix.
    #[test]
    fn which_finds_a_ubiquitous_binary() {
        let name = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(which(name).is_some(), "which should locate `{name}` on PATH");
    }
}
