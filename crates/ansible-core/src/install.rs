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
    /// Which ansible-core this is. `None` when no install was found, and rules that gate on
    /// it must say at their own call site what they do with that — T-138.
    pub version: Option<Version>,
    /// The interpreter this Ansible runs on — the value `ansible_playbook_python` would hold
    /// for a run launched from this install. Reconstructed, never read from a Python process:
    /// see [`venv_interpreter`]. `None` is normal, and a consumer must treat it as "unknown",
    /// not "no Python" — T-051.
    pub python: Option<PathBuf>,
    /// Which path found `package_dir`, and what the whole detection cost. Startup is the
    /// only place this runs, and it used to be unmeasured — T-084.
    pub source: Source,
    pub detect_ms: f64,
}

/// An ansible-core release, ordered. Pre-release suffixes (`2.22.0.dev0`, `2.19.0rc1`) are
/// dropped rather than ordered: every version gate we have is "since X.Y", and a dev build of
/// X.Y.Z already behaves like X.Y.Z for those purposes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// The numeric head of a version string; `None` unless it starts with a digit.
    pub fn parse(s: &str) -> Option<Self> {
        let mut parts = s.trim().split('.');
        Some(Self {
            major: leading_number(parts.next()?)?,
            minor: parts.next().and_then(leading_number).unwrap_or(0),
            patch: parts.next().and_then(leading_number).unwrap_or(0),
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn leading_number(s: &str) -> Option<u32> {
    s.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// `__version__ = '2.22.0.dev0'` out of `<package_dir>/release.py`. This is the same constant
/// `ansible --version` prints (`option_helpers.py:288`), for the price of one file read — the
/// subprocess costs seconds cold and doesn't run on Windows at all (T-084).
fn version_from_release_py(text: &str) -> Option<Version> {
    let line = text.lines().find(|l| l.trim_start().starts_with("__version__"))?;
    let (_, rhs) = line.split_once('=')?;
    let literal = rhs.trim().trim_matches(|c| c == '\'' || c == '"');
    Version::parse(literal)
}

/// The first line of `ansible --version`: `ansible [core 2.21.2]`, or bare `ansible 2.9.27`
/// before the collection split.
fn version_from_version_output(text: &str) -> Option<Version> {
    let first = text.lines().next()?;
    let token = first
        .split(|c: char| c.is_whitespace() || c == '[' || c == ']')
        .find(|t| t.starts_with(|c: char| c.is_ascii_digit()))?;
    Version::parse(token)
}

/// `ansible_playbook_python` is `sys.executable`, which only exists inside a running Python —
/// and starting one is the thing detection exists to avoid (T-084: 3.6 s cold, and it does not
/// run on Windows). Every source below therefore *reconstructs* the path, and each one is
/// checked against this before it is believed: whatever we name must be a file, and must be a
/// python. It rejects the two shebang forms that name something else — `#!/usr/bin/env python3`
/// (not a path) and the `#!/bin/sh` + `exec` form pip writes when the prefix has spaces — and
/// the `(main, ...)` group of a `--version` line too old to carry the interpreter.
///
/// Name only — existence is the caller's check, since only some of the sources can be stale.
fn names_a_python(p: &Path) -> bool {
    p.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.starts_with("python"))
}

/// The interpreter from the venv layout, which is the one source available on every platform.
///
/// `find_site_packages` only ever returns `<prefix>/lib/pythonX.Y/site-packages/ansible` or
/// `<prefix>/Lib/site-packages/ansible`, so the prefix is the parent of the `lib` component —
/// found by name rather than by popping a fixed depth, since the two layouts differ by one
/// level. Derived from `package_dir` and **not** from the `ansible` executable's parent: for a
/// uv tool install those are different directories, the shim living outside the venv
/// (`install.rs` override comment, T-051). From the package dir a uv install gives
/// `.../uv/tools/ansible-core/bin/python`, which is what its console script's shebang says too.
fn venv_interpreter(pkg: &Path) -> Option<PathBuf> {
    let prefix = venv_prefix(pkg)?;
    let candidates: &[&str] = if cfg!(windows) {
        &["Scripts/python.exe"]
    } else {
        &["bin/python3", "bin/python"]
    };
    candidates
        .iter()
        .map(|c| prefix.join(c))
        .find(|p| p.is_file())
}

/// The `lib` component's parent. Found by name because the two layouts differ by a level —
/// `lib/pythonX.Y/site-packages` against Windows' `Lib/site-packages` — so a fixed pop depth
/// would be right on exactly one platform. Also lands correctly on a distro-packaged
/// `/usr/lib/python3/dist-packages/ansible`, whose prefix really is `/usr`.
fn venv_prefix(pkg: &Path) -> Option<&Path> {
    let mut cur = pkg.parent();
    while let Some(p) = cur {
        if p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("lib")) {
            return p.parent();
        }
        cur = p.parent();
    }
    None
}

/// The interpreter a console script names on its first line. Preferred over the layout probe
/// where it exists, because it is what the script actually execs — pip and uv bake the absolute
/// path in at install time. Unix only in practice: a Windows console script is an `.exe` with
/// the path embedded in the binary, and the read below simply finds no `#!`.
fn shebang_interpreter(exe: &Path) -> Option<PathBuf> {
    use std::io::Read;
    let mut buf = [0u8; 256];
    let n = std::fs::File::open(exe).ok()?.read(&mut buf).ok()?;
    // The baked-in path can outlive the venv it names, so this source is checked for existence
    // as well as shape; a moved venv falls through to the layout probe instead of lying.
    parse_shebang(&String::from_utf8_lossy(&buf[..n])).filter(|p| p.is_file())
}

/// The kernel reads a shebang as an interpreter plus at most one argument, so the path is the
/// first token — which is also what makes `#!/usr/bin/env python3` fall out as `env`, and
/// `#!/usr/bin/python3 -sE` (RPM's form) keep the interpreter without the flags.
fn parse_shebang(head: &str) -> Option<PathBuf> {
    let line = head.lines().next()?.strip_prefix("#!")?;
    let path = PathBuf::from(line.split_whitespace().next()?);
    names_a_python(&path).then_some(path)
}

/// `python version = 3.11.2 (main, ...) [GCC 12.2.0] (/usr/bin/python3)` — the trailing group is
/// `sys.executable` verbatim, the one field of `--version` that reports it. It was not always
/// there, and on a core that predates it the last group is `(main, ...)` from `sys.version`,
/// which [`is_python_file`] rejects.
fn interpreter_from_version_line(value: &str) -> Option<PathBuf> {
    let start = value.rfind('(')?;
    let path = PathBuf::from(value[start + 1..].trim_end_matches(')').trim());
    names_a_python(&path).then_some(path)
}

/// How `package_dir` was found. The cost difference between these is three orders of
/// magnitude, so "which one won" is the number worth logging.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Source {
    #[default]
    NotFound,
    /// `ansibleLsp.ansiblePath`.
    Override,
    /// Walked up from the `ansible` executable on PATH.
    PathWalkUp,
    /// A uv/pipx tool venv.
    ToolInstall,
    /// The `ansible --version` subprocess — seconds when Python's import cache is cold.
    VersionCommand,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not-found",
            Self::Override => "override",
            Self::PathWalkUp => "path-walk-up",
            Self::ToolInstall => "tool-install",
            Self::VersionCommand => "version-command",
        }
    }
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

    /// The result if detection has already run — never starting it. Anything on a request path
    /// must use this: [`detect`](Self::detect) is `get_or_init`, so the first caller pays the
    /// whole cost, and on the message pump that is the 3.6 s freeze T-084 measured.
    pub fn detected() -> Option<&'static Self> {
        DETECTED.get()
    }

    fn run() -> Self {
        // Fast path: derive everything from the filesystem. `ansible --version` is
        // authoritative but costs seconds of Python startup when cold (3.6 s measured, T-084)
        // — and on Windows it crashes outright (Ansible's control node isn't supported
        // there), so once the filesystem has found the package we must NOT fall through to it.
        let started = std::time::Instant::now();
        let fast = Self::from_filesystem();
        let mut install = if fast.package_dir.is_some() {
            fast
        } else {
            Self::from_version_command().unwrap_or(fast)
        };
        install.detect_ms = started.elapsed().as_secs_f64() * 1e3;
        install
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
                install.source = Source::Override;
            }
        }

        // Kept for the shebang read at the end — `which` is a PATH scan plus a canonicalize,
        // so the interpreter derivation borrows this one rather than repeating it.
        let mut exe_path = None;
        if install.package_dir.is_none() {
            if let Some(exe) = which("ansible") {
                exe_path = Some(exe.clone());
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
                            install.source = Source::PathWalkUp;
                        }
                    }
                }
            }
        }

        // uv/pipx can put the `ansible` executable behind a shim outside the venv, so the
        // walk-up above can't reach the package. Platform-dependent: on Linux uv symlinks
        // `~/.local/bin/ansible` into the tool venv and `which` canonicalizes, so the
        // walk-up wins and this never fires (measured, T-084); on Windows uv writes a real
        // trampoline `.exe` and this is the only thing that finds the install. Probe their
        // conventional tool-install dirs
        // directly — this is what makes a `uv tool install ansible-core` just work, with no
        // env var and no working `ansible` CLI (which crashes on Windows anyway).
        if install.package_dir.is_none() {
            if let Some(pkg) = find_tool_install() {
                let bundled = pkg.with_file_name("ansible_collections");
                if bundled.is_dir() {
                    install.collection_roots.push(bundled);
                }
                install.package_dir = Some(pkg);
                install.source = Source::ToolInstall;
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

        // One read, whichever of the three branches above found the package.
        install.version = install.package_dir.as_ref().and_then(|pkg| {
            let text = std::fs::read_to_string(pkg.join("release.py")).ok()?;
            version_from_release_py(&text)
        });

        // Shebang first where there is one — it is what the script execs. The layout probe is
        // the answer everywhere else: the override and tool-install branches never resolved an
        // exe, and a Windows console script has no shebang to read.
        install.python = exe_path
            .as_deref()
            .and_then(shebang_interpreter)
            .or_else(|| install.package_dir.as_deref().and_then(venv_interpreter));
        install
    }

    fn from_version_command() -> Option<Self> {
        let out = std::process::Command::new("ansible")
            .arg("--version")
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut install = Self {
            version: version_from_version_output(&text),
            ..Default::default()
        };

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
                    install.source = Source::VersionCommand;
                }
                "python version" => install.python = interpreter_from_version_line(value),
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
        // Every install ships `release.py`; a package dir with no version means the read or
        // the parse broke, not that this Ansible is unversioned.
        assert!(i.version.is_some(), "a detected install must carry a version");
    }

    /// Verbatim from `lib/ansible/release.py` — the whole file, because it is this small and
    /// the parse has to survive the module docstring and the two constants beside `__version__`.
    const RELEASE_PY: &str = r#"# Copyright: (c) 2017 Ansible Project
# GNU General Public License v3.0+

from __future__ import annotations

__version__ = '2.22.0.dev0'
__author__ = 'Ansible, Inc.'
__codename__ = "Fool in the Rain"
"#;

    #[test]
    fn reads_the_version_out_of_release_py() {
        assert_eq!(
            version_from_release_py(RELEASE_PY),
            Some(Version { major: 2, minor: 22, patch: 0 })
        );
        // Double quotes and a release build, both of which ship.
        assert_eq!(
            version_from_release_py("__version__ = \"2.19.3\"\n"),
            Some(Version { major: 2, minor: 19, patch: 3 })
        );
        assert_eq!(version_from_release_py("__author__ = 'Ansible, Inc.'\n"), None);
    }

    /// A pre-release is the version it is heading for: gates read "since 2.19", and `2.19.0rc1`
    /// is on the far side of that. Nothing here may parse to a *lower* version than the release.
    #[test]
    fn pre_release_suffixes_parse_to_their_release() {
        let cases = [
            ("2.22.0.dev0", Version { major: 2, minor: 22, patch: 0 }),
            ("2.19.0rc1", Version { major: 2, minor: 19, patch: 0 }),
            ("2.18.0b1", Version { major: 2, minor: 18, patch: 0 }),
            ("2.21.2", Version { major: 2, minor: 21, patch: 2 }),
            ("2.19", Version { major: 2, minor: 19, patch: 0 }),
        ];
        for (text, want) in cases {
            assert_eq!(Version::parse(text), Some(want), "parsing {text}");
        }
        assert_eq!(Version::parse("devel"), None);
        assert_eq!(Version::parse(""), None);
    }

    #[test]
    fn version_gates_compare_the_way_since_x_y_reads() {
        let v = |s: &str| Version::parse(s).unwrap();
        assert!(v("2.19.0") >= v("2.19"));
        assert!(v("2.22.0.dev0") > v("2.19.3"));
        assert!(v("2.16.14") < v("2.19"));
        assert!(v("2.9.27") < v("2.10.0"));
    }

    /// Both shapes of the first line: post-2.10 `[core X]`, and the bare pre-split form.
    #[test]
    fn reads_the_version_off_the_version_command() {
        let out = "ansible [core 2.21.2]\n  config file = None\n";
        assert_eq!(
            version_from_version_output(out),
            Some(Version { major: 2, minor: 21, patch: 2 })
        );
        assert_eq!(
            version_from_version_output("ansible 2.9.27\n"),
            Some(Version { major: 2, minor: 9, patch: 27 })
        );
        assert_eq!(version_from_version_output(""), None);
    }

    /// The prefix is what makes the derivation work off `package_dir` rather than off the
    /// `ansible` exe, so it has to hold for every layout `find_site_packages` can return —
    /// including the uv tool install, where the exe's own parent is the wrong answer (T-051).
    #[test]
    fn the_venv_prefix_is_found_in_every_layout_we_accept() {
        let cases = [
            ("/venv/lib/python3.11/site-packages/ansible", Some("/venv")),
            ("C:/venv/Lib/site-packages/ansible", Some("C:/venv")),
            ("/usr/lib/python3/dist-packages/ansible", Some("/usr")),
            (
                "/home/o/.local/share/uv/tools/ansible-core/lib/python3.13/site-packages/ansible",
                Some("/home/o/.local/share/uv/tools/ansible-core"),
            ),
            ("/nowhere/site-packages/ansible", None),
        ];
        for (pkg, want) in cases {
            let got = venv_prefix(Path::new(pkg)).map(|p| p.display().to_string());
            assert_eq!(got.as_deref(), want, "prefix of {pkg}");
        }
    }

    /// The shebang forms that are *not* `sys.executable`, each of which a naive
    /// "take the rest of the line" read would hand back as an interpreter path.
    #[test]
    fn a_shebang_yields_an_interpreter_only_when_it_names_one() {
        let py = |s: &str| parse_shebang(s).map(|p| p.display().to_string());
        assert_eq!(
            py("#!/home/o/.local/share/uv/tools/ansible-core/bin/python\n"),
            Some("/home/o/.local/share/uv/tools/ansible-core/bin/python".into())
        );
        // RPM ships flags on the interpreter line; the kernel splits them off and so do we.
        assert_eq!(py("#!/usr/bin/python3 -sE\n"), Some("/usr/bin/python3".into()));
        // `env` is the interpreter here, and the python is an argument — not a path we know.
        assert_eq!(py("#!/usr/bin/env python3\n"), None);
        // pip's form when the prefix has spaces: the shell runs, then re-execs.
        assert_eq!(py("#!/bin/sh\n'''exec' /path/python \"$0\" \"$@\"\n"), None);
        assert_eq!(py("MZ\u{0}\u{0}binary junk"), None);
        assert_eq!(py(""), None);
    }

    /// The `python version` line carries `sys.executable` in a trailing group — but only since
    /// it was added, and before that the last group is `sys.version`'s build stamp.
    #[test]
    fn the_python_version_line_gives_up_the_interpreter_only_when_it_has_one() {
        let line = "3.11.2 (main, Mar 13 2023, 12:18:29) [GCC 12.2.0] (/usr/bin/python3)";
        assert_eq!(
            interpreter_from_version_line(line).map(|p| p.display().to_string()),
            Some("/usr/bin/python3".into())
        );
        // Older core: the last group is the build stamp, and taking it would be a bogus path.
        let old = "3.8.10 (default, Nov 14 2022, 12:59:47) [GCC 9.4.0]";
        assert_eq!(interpreter_from_version_line(old), None);
        assert_eq!(interpreter_from_version_line("3.8.10"), None);
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
