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

use std::path::{Path, PathBuf};

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
