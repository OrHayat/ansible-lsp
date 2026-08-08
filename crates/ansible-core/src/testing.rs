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
    /// A declared dir makes its **ancestors** dirs too, not just itself. Without that,
    /// `with_dirs(&[], &["/p/a/b/empty"])` left `/p/a` reporting as nonexistent while
    /// `read_dir("/p")` happily listed it — the two answers contradicting each other.
    fn kind(&self, p: &Path) -> Option<Kind> {
        if self.files.contains_key(p) {
            Some(Kind::File)
        } else if self.dirs.contains(p)
            || self.dirs.iter().chain(self.files.keys()).any(|k| k.starts_with(p) && k != p)
        {
            Some(Kind::Dir)
        } else {
            None
        }
    }
    fn read(&self, p: &Path) -> Option<String> {
        self.files.get(p).cloned()
    }
    /// Subdirectories are listed as well as files. A glob descends a wildcard segment by
    /// filtering this on [`Kind::Dir`] (`glob.rs:80`), so a files-only listing would make
    /// every `*/name.yml` pattern silently match nothing.
    fn read_dir(&self, p: &Path) -> Vec<(PathBuf, Kind)> {
        let mut out: BTreeMap<PathBuf, Kind> = BTreeMap::new();
        for k in self.files.keys().chain(self.dirs.iter()) {
            let Ok(rest) = k.strip_prefix(p) else { continue };
            let Some(first) = rest.components().next() else { continue };
            let child = p.join(first);
            // A key is a file only when the whole of it was consumed by that one component.
            let kind = if child == *k && self.files.contains_key(k) { Kind::File } else { Kind::Dir };
            out.insert(child, kind);
        }
        out.into_iter().collect()
    }
    /// Nothing in an in-memory tree is a symlink, so a path is its own identity — but
    /// `canonicalize` also collapses `.` and `..`, and that half has to be done here.
    /// `Fs::same_file` and the visited-sets in `vars.rs`/`mutation.rs` are all built on this,
    /// so leaving `/p/roles/../site.yml` unresolved makes one file count as two: a cycle
    /// guard stops matching and a file gets walked twice.
    fn canonical(&self, p: &Path) -> Option<PathBuf> {
        let p = normalise(p);
        self.exists(&p).then_some(p)
    }
    fn walk(&self, root: &Path) -> Vec<(PathBuf, Vec<String>)> {
        let mut dirs: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
        dirs.insert(root.to_path_buf(), Vec::new());
        // Every intermediate dir between root and `d` exists too — for a declared empty dir
        // exactly as for the dirs a file's own path implies.
        let reach = |d: &Path, dirs: &mut BTreeMap<PathBuf, Vec<String>>| {
            let mut cur = d.to_path_buf();
            while cur != *root {
                dirs.entry(cur.clone()).or_default();
                let Some(parent) = cur.parent() else { break };
                cur = parent.to_path_buf();
            }
        };
        for d in self.dirs.iter().filter(|d| d.starts_with(root)) {
            reach(d, &mut dirs);
        }
        for k in self.files.keys().filter(|k| k.starts_with(root)) {
            let dir = k.parent().unwrap().to_path_buf();
            reach(&dir, &mut dirs);
            dirs.entry(dir)
                .or_default()
                .push(k.file_name().unwrap().to_str().unwrap().to_string());
        }
        dirs.into_iter().collect()
    }
}

/// Lexical `.`/`..` removal — the part of `canonicalize` that isn't symlink resolution.
/// `.` alone would need no help (`Path`'s comparison already drops `CurDir` components), but
/// `..` does: `/p/roles/../site.yml` and `/p/site.yml` are different paths to `Path`, and the
/// same file to the real filesystem.
fn normalise(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
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

/// The fake's own tests. A bug here doesn't fail a test — it makes tests *pass* against
/// wrong code, which is how all three of these were found: by auditing `MemFs` against
/// `StdFs` rather than by anything going red.
#[cfg(test)]
mod tests {
    use super::*;

    /// `glob::expand` filters a directory listing on [`Kind::Dir`] to descend a wildcard
    /// segment (`glob.rs:80`). This used to report every entry as [`Kind::File`], so every
    /// `*/name.yml` pattern matched nothing and any glob test on the fake passed vacuously.
    #[test]
    fn read_dir_reports_subdirectories_so_a_glob_can_descend() {
        let fs = MemFs::new(&[("/p/site.yml", ""), ("/p/roles/r/tasks/main.yml", "")]);
        let mut got = fs.read_dir(Path::new("/p"));
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            got,
            vec![
                (PathBuf::from("/p/roles"), Kind::Dir),
                (PathBuf::from("/p/site.yml"), Kind::File),
            ]
        );
        // A file has no entries, rather than yielding itself.
        assert!(fs.read_dir(Path::new("/p/site.yml")).is_empty());
    }

    /// A declared empty dir must exist all the way up. `kind` and `walk` used to consider
    /// only the exact path declared, so `/p/a` read as nonexistent while `read_dir("/p")`
    /// listed it — the fake contradicting itself.
    #[test]
    fn a_declared_empty_dir_brings_its_ancestors_with_it() {
        let fs = MemFs::with_dirs(&[("/p/site.yml", "")], &["/p/a/b/empty"]);
        for p in ["/p/a", "/p/a/b", "/p/a/b/empty"] {
            assert_eq!(fs.kind(Path::new(p)), Some(Kind::Dir), "{p}");
        }
        let walked: Vec<PathBuf> = fs.walk(Path::new("/p")).into_iter().map(|(d, _)| d).collect();
        assert_eq!(
            walked,
            vec![
                PathBuf::from("/p"),
                PathBuf::from("/p/a"),
                PathBuf::from("/p/a/b"),
                PathBuf::from("/p/a/b/empty"),
            ]
        );
    }

    /// [`Fs::same_file`] and the visited-sets in `vars.rs`/`mutation.rs` are built on
    /// `canonical`, so an unresolved `..` makes one file count as two: a cycle guard stops
    /// matching and a file gets walked twice.
    #[test]
    fn canonical_collapses_dot_segments_so_one_file_is_not_two() {
        let fs = MemFs::new(&[("/p/site.yml", ""), ("/p/roles/r/tasks/main.yml", "")]);
        let want = fs.canonical(Path::new("/p/site.yml")).expect("plain path resolves");
        for spelling in [
            "/p/./site.yml",
            "/p/roles/../site.yml",
            "/p/roles/r/../../site.yml",
        ] {
            assert_eq!(fs.canonical(Path::new(spelling)).as_ref(), Some(&want), "{spelling}");
            assert!(fs.same_file(Path::new("/p/site.yml"), Path::new(spelling)), "{spelling}");
        }
        // Collapsing must not turn into clamping: climbing out of the tree still misses.
        assert_eq!(fs.canonical(Path::new("/p/../elsewhere.yml")), None);
    }
}
