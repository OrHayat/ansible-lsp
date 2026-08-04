//! The crate's one door to the filesystem.
//!
//! Everything that touches disk — existence probes, reads, directory listings,
//! symlink resolution — goes through [`Fs`], so that "how much work did a pass do?"
//! is answered by the door rather than by remembering every call site. That question
//! was the reason this exists: T-076's first attempt measured the walk with counters
//! bolted onto two call sites, reported 561 probes, and was wrong by 4× — `strace`
//! found 2340, the rest hiding in `glob`, `include_vars`, `yaml_files` and
//! `canonicalize`. A seam can't drift the way a hand-maintained counter list does.
//!
//! [`StdFs`] is the real filesystem and the default everywhere. The second
//! implementation — a pass-scoped memo — is T-085; nothing here caches anything.

use std::path::{Path, PathBuf};

/// What lives at a path. One primitive rather than three predicates, because a single
/// `stat` answers all three questions — so an implementation that remembers answers
/// keeps one map, and remembers *absence* too. That last part matters more than it
/// looks: 724 of the 1455 probes in a demo scan are misses, so caching only the hits
/// would leave half the work on the floor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    File,
    Dir,
    /// A symlink to nowhere, a socket, a fifo — exists, but neither of the above.
    Other,
}

pub trait Fs: Send + Sync {
    /// `None` when nothing is there.
    fn kind(&self, p: &Path) -> Option<Kind>;

    fn is_file(&self, p: &Path) -> bool {
        self.kind(p) == Some(Kind::File)
    }

    fn is_dir(&self, p: &Path) -> bool {
        self.kind(p) == Some(Kind::Dir)
    }

    fn exists(&self, p: &Path) -> bool {
        self.kind(p).is_some()
    }

    fn read(&self, p: &Path) -> Option<String>;

    /// One directory's entries, not descending. Order is the filesystem's — callers
    /// that need determinism sort.
    fn read_dir(&self, p: &Path) -> Vec<PathBuf>;

    /// Every directory at-or-under `root`, each with its file basenames. Mirrors
    /// `os.walk(followlinks=True)`, which is the shape [`crate::include_vars`] needs
    /// to replay the module's own directory semantics.
    fn walk(&self, root: &Path) -> Vec<(PathBuf, Vec<String>)>;

    /// Symlink-resolved identity — what says "these two paths are the same file", for
    /// cycle detection and dependency sets. `None` when the path doesn't exist.
    ///
    /// Costly on the real filesystem: `realpath` interrogates *every* component, so
    /// 711 of 712 `readlink` calls in a demo scan return "not a symlink" — the shared
    /// `/mnt/c/Users/…` prefix, re-walked once per file. An implementation that
    /// resolves per directory turns that into one lookup per new file (T-085).
    fn canonical(&self, p: &Path) -> Option<PathBuf>;

    /// Identity for the default paths, so `MemFs`-style fakes need not resolve links.
    fn same_file(&self, a: &Path, b: &Path) -> bool {
        match (self.canonical(a), self.canonical(b)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        }
    }
}

/// The real filesystem. What every caller gets unless it opts into something else.
pub struct StdFs;

impl Fs for StdFs {
    fn kind(&self, p: &Path) -> Option<Kind> {
        let m = std::fs::metadata(p).ok()?;
        Some(if m.is_file() {
            Kind::File
        } else if m.is_dir() {
            Kind::Dir
        } else {
            Kind::Other
        })
    }

    fn read(&self, p: &Path) -> Option<String> {
        std::fs::read_to_string(p).ok()
    }

    fn read_dir(&self, p: &Path) -> Vec<PathBuf> {
        match std::fs::read_dir(p) {
            Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
            Err(_) => Vec::new(),
        }
    }

    fn walk(&self, root: &Path) -> Vec<(PathBuf, Vec<String>)> {
        fn descend(fs: &StdFs, dir: &Path, out: &mut Vec<(PathBuf, Vec<String>)>) {
            let mut files = Vec::new();
            let mut subdirs = Vec::new();
            for p in fs.read_dir(dir) {
                if p.is_dir() {
                    subdirs.push(p);
                } else if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
                    files.push(n.to_string());
                }
            }
            out.push((dir.to_path_buf(), files));
            for s in subdirs {
                descend(fs, &s, out);
            }
        }
        let mut out = Vec::new();
        descend(self, root, &mut out);
        out
    }

    fn canonical(&self, p: &Path) -> Option<PathBuf> {
        p.canonicalize().ok()
    }
}
