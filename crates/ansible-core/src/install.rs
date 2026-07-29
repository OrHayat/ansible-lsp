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

impl AnsibleInstall {
    /// Detected once per process by shelling out to `ansible --version` (~300 ms).
    pub fn detect() -> &'static Self {
        DETECTED.get_or_init(Self::run)
    }

    fn run() -> Self {
        // Fast path: derive everything from the filesystem. `ansible --version` is
        // authoritative but costs ~500 ms of Python startup, which lands straight on
        // the first request.
        let fast = Self::from_filesystem();
        if fast.package_dir.is_some() && !fast.collection_roots.is_empty() {
            return fast;
        }
        Self::from_version_command().unwrap_or(fast)
    }

    /// Locate the ansible package by resolving the `ansible` executable, plus the
    /// standard collection directories. No subprocess.
    fn from_filesystem() -> Self {
        let mut install = Self::default();

        if let Some(exe) = which("ansible") {
            // .../libexec/bin/ansible -> .../libexec/lib/python3.14/site-packages/ansible
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

        let mut roots = Vec::new();
        if let Ok(env) = std::env::var("ANSIBLE_COLLECTIONS_PATH") {
            roots.extend(env.split(':').map(PathBuf::from));
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
}

/// First `name` on PATH, with symlinks resolved.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").ok()?;
    path.split(':')
        .map(|d| PathBuf::from(d).join(name))
        .find(|p| p.is_file())
        .and_then(|p| std::fs::canonicalize(p).ok())
}

/// `<prefix>/lib/python3.X/site-packages/ansible`, whichever python version.
fn find_site_packages(prefix: &Path) -> Option<PathBuf> {
    let lib = prefix.join("lib");
    for entry in std::fs::read_dir(lib).ok()?.flatten() {
        let candidate = entry.path().join("site-packages/ansible");
        if candidate.join("modules").is_dir() {
            return Some(candidate);
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
}
