//! Test-only helpers for building a project tree on disk.
//!
//! T-077: tests used to resolve against a private repo under `$HOME`, guarded with
//! `else { return }`, so they skipped silently on any other machine — a green run proved
//! nothing on CI or a fresh checkout, and one of them panicked outright on Windows, where
//! `HOME` is unset.
//!
//! A tree is written from string literals **at the call site**, so a test states exactly what
//! it depends on and there is no committed fixture directory to drift out of sync with the
//! assertions it feeds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::fs::{Fs, Kind};

/// An in-memory tree: path -> contents, declared as string literals. The one `Fs` fake for
/// the whole crate (T-134) — it used to be a private `struct` inside `include_vars.rs`'s test
/// module, so every other test module built a real directory instead.
///
/// A key is a file; a proper prefix of a key is a directory. Directories that hold no files
/// can't be seen by that prefix rule — a role with an empty `tasks/` is a real case — so they
/// are carried explicitly in `dirs`.
///
/// Prefer this to [`tree`] wherever the code under test takes a `&dyn Fs` (`discover_with`,
/// `load`, `resolve_in`): no I/O, no temp directory left behind, and no unique-name rule.
/// [`tree`] remains for the paths that hardcode [`crate::fs::StdFs`].
pub struct MemFs {
    files: BTreeMap<PathBuf, String>,
    dirs: BTreeSet<PathBuf>,
}

impl MemFs {
    pub fn new(files: &[(&str, &str)]) -> Self {
        Self::with_dirs(files, &[])
    }

    /// `dirs` names directories that exist while holding no files of their own.
    pub fn with_dirs(files: &[(&str, &str)], dirs: &[&str]) -> Self {
        Self {
            files: files.iter().map(|(p, c)| (PathBuf::from(p), c.to_string())).collect(),
            dirs: dirs.iter().map(PathBuf::from).collect(),
        }
    }
}

impl Fs for MemFs {
    fn kind(&self, p: &Path) -> Option<Kind> {
        if self.files.contains_key(p) {
            Some(Kind::File)
        } else if self.dirs.contains(p) || self.files.keys().any(|k| k.starts_with(p) && k != p) {
            Some(Kind::Dir)
        } else {
            None
        }
    }
    fn read(&self, p: &Path) -> Option<String> {
        self.files.get(p).cloned()
    }
    fn read_dir(&self, p: &Path) -> Vec<(PathBuf, Kind)> {
        self.files
            .keys()
            .filter(|k| k.parent() == Some(p))
            .map(|k| (k.clone(), Kind::File))
            .collect()
    }
    /// Nothing in an in-memory tree is a symlink, so a path is its own identity.
    fn canonical(&self, p: &Path) -> Option<PathBuf> {
        self.exists(p).then(|| p.to_path_buf())
    }
    fn walk(&self, root: &Path) -> Vec<(PathBuf, Vec<String>)> {
        let mut dirs: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
        dirs.insert(root.to_path_buf(), Vec::new());
        for d in self.dirs.iter().filter(|d| d.starts_with(root)) {
            dirs.entry(d.clone()).or_default();
        }
        for k in self.files.keys().filter(|k| k.starts_with(root)) {
            let dir = k.parent().unwrap().to_path_buf();
            // Every intermediate dir between root and the file exists too.
            let mut d = dir.clone();
            while d != *root {
                dirs.entry(d.clone()).or_default();
                d = d.parent().unwrap().to_path_buf();
            }
            dirs.entry(dir)
                .or_default()
                .push(k.file_name().unwrap().to_str().unwrap().to_string());
        }
        dirs.into_iter().collect()
    }
}

/// An `Fs` holding exactly one `ansible.cfg`, whatever path is asked for — the config tests
/// care about parsing, not about where the file sits. `None` means no config at all, which is
/// its own case: an env override has to apply with no `ansible.cfg` present.
pub struct CfgFs(pub Option<String>);

impl CfgFs {
    /// A project whose `ansible.cfg` holds `text`.
    pub fn some(text: &str) -> Self {
        Self(Some(text.to_string()))
    }
    /// A project with no `ansible.cfg` at all.
    pub fn none() -> Self {
        Self(None)
    }
}

impl Fs for CfgFs {
    fn kind(&self, _p: &Path) -> Option<Kind> {
        self.0.as_ref().map(|_| Kind::File)
    }
    fn read(&self, _p: &Path) -> Option<String> {
        self.0.clone()
    }
    fn read_dir(&self, _p: &Path) -> Vec<(PathBuf, Kind)> {
        Vec::new()
    }
    fn walk(&self, _root: &Path) -> Vec<(PathBuf, Vec<String>)> {
        Vec::new()
    }
    fn canonical(&self, p: &Path) -> Option<PathBuf> {
        self.0.as_ref().map(|_| p.to_path_buf())
    }
}

/// Write `files` (relative path, contents) into a fresh directory and return its root.
///
/// `name` must be unique per test: the directory is reused across runs and wiped first, so
/// two tests sharing a name would race under the default parallel harness.
///
/// Parent directories are created as needed, so a tree is just its leaves. An empty string
/// means an empty file, which is enough whenever a case only needs a path to *exist* —
/// resolution asks the filesystem, not the parser.
pub fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("ansible-lsp-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    for (rel, contents) in files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("mkdir {}: {e}", parent.display()));
        }
        std::fs::write(&path, contents)
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
    // A directory-only entry has no leaf to write; callers that need one pass a file in it.
    std::fs::create_dir_all(&root).expect("root");
    root
}

/// [`tree`] plus an `ansible.cfg` at the root, which is what makes it a *project* — without
/// one there is no `project_root`, so `playbook_dir` and the role search path have no values.
pub fn project(name: &str, cfg: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = tree(name, files);
    std::fs::write(root.join("ansible.cfg"), cfg).expect("ansible.cfg");
    root
}

/// Assert a path exists, naming it — a tree typo otherwise surfaces as a confusing
/// resolution failure several frames away.
pub fn present(p: &Path) {
    assert!(p.exists(), "fixture missing: {}", p.display());
}
