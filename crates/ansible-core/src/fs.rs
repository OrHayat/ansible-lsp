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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::Instant;

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

    /// [`kind`](Fs::kind) without following a final symlink — one `lstat`. A symlink reports
    /// [`Kind::Other`] whatever it points at, which is the signal "resolve this properly".
    ///
    /// Defaults to `kind`, which is exactly right for a tree that has no symlinks — an
    /// in-memory fake, say. Only a real filesystem needs to override it.
    fn symlink_kind(&self, p: &Path) -> Option<Kind> {
        self.kind(p)
    }

    fn read(&self, p: &Path) -> Option<String>;

    /// One directory's entries with what each one *is*, not descending. Order is the
    /// filesystem's — callers that need determinism sort.
    ///
    /// The kind rides along because the directory read already knows it: `getdents64`
    /// carries `d_type`, `FindNextFile` carries the attributes. Returning bare paths threw
    /// that away and made every caller stat each entry to re-learn what the kernel had
    /// just told us. A memoizing implementation can seed its existence map from this for
    /// free.
    fn read_dir(&self, p: &Path) -> Vec<(PathBuf, Kind)>;

    /// Entry paths only, for callers that don't care what they are.
    fn read_dir_paths(&self, p: &Path) -> Vec<PathBuf> {
        self.read_dir(p).into_iter().map(|(p, _)| p).collect()
    }

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

/// So a caller can keep a handle on an implementation it also hands to something else —
/// the point of [`Counting`] being a decorator is reading its tallies afterwards.
impl<T: Fs + ?Sized> Fs for std::sync::Arc<T> {
    fn kind(&self, p: &Path) -> Option<Kind> {
        (**self).kind(p)
    }
    fn symlink_kind(&self, p: &Path) -> Option<Kind> {
        (**self).symlink_kind(p)
    }
    fn read(&self, p: &Path) -> Option<String> {
        (**self).read(p)
    }
    fn read_dir(&self, p: &Path) -> Vec<(PathBuf, Kind)> {
        (**self).read_dir(p)
    }
    fn walk(&self, root: &Path) -> Vec<(PathBuf, Vec<String>)> {
        (**self).walk(root)
    }
    fn canonical(&self, p: &Path) -> Option<PathBuf> {
        (**self).canonical(p)
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

    fn symlink_kind(&self, p: &Path) -> Option<Kind> {
        let m = std::fs::symlink_metadata(p).ok()?;
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

    fn read_dir(&self, p: &Path) -> Vec<(PathBuf, Kind)> {
        let Ok(rd) = std::fs::read_dir(p) else { return Vec::new() };
        rd.flatten()
            .map(|e| {
                let path = e.path();
                // `file_type()` comes free with the listing but does NOT follow symlinks,
                // where `is_dir()` does. Resolving those with a real stat keeps a symlinked
                // role directory walkable; they are rare enough for that to stay cheap.
                let kind = match e.file_type() {
                    Ok(t) if t.is_dir() => Some(Kind::Dir),
                    Ok(t) if t.is_file() => Some(Kind::File),
                    _ => self.kind(&path),
                };
                (path, kind.unwrap_or(Kind::Other))
            })
            .collect()
    }

    fn walk(&self, root: &Path) -> Vec<(PathBuf, Vec<String>)> {
        fn descend(fs: &StdFs, dir: &Path, out: &mut Vec<(PathBuf, Vec<String>)>) {
            let mut files = Vec::new();
            let mut subdirs = Vec::new();
            for (p, kind) in fs.read_dir(dir) {
                if kind == Kind::Dir {
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

// ------------------------------------------------------------------ counting (T-085)

/// One operation's tally. Atomics, not a lock: a mutex on every filesystem call would
/// serialise the concurrency the walk is built to allow, and it would double the locking a
/// memoizing [`Fs`] already does — noise against a 540 µs stat on a 9p mount, 20–40% against
/// a ~50–100 ns memo hit, which is the fast path the memo exists to create. Nothing branches
/// on these mid-run; only the totals are read at the end, so `Relaxed` is enough.
/// One operation's tally.
///
/// There is no `disk` field: one layer cannot know whether the thing below it went to the
/// filesystem, and a single wrapper would report `disk == calls` always. The redundancy
/// number comes from *stacking* instead — a [`Counting`] inside the memo counts syscalls, one
/// outside counts asks, and the pair is the ratio.
#[derive(Default, Debug)]
pub struct Counter {
    /// Times this layer was asked.
    pub calls: AtomicUsize,
    /// Answers of "nothing there". Half of all probes in a real scan — the number that
    /// catches a cache which remembers only hits.
    pub misses: AtomicUsize,
    /// Time spent below this layer.
    pub nanos: AtomicU64,
}

impl Counter {
    fn record(&self, elapsed: std::time::Duration, missing: bool) {
        self.calls.fetch_add(1, Relaxed);
        self.nanos.fetch_add(elapsed.as_nanos() as u64, Relaxed);
        if missing {
            self.misses.fetch_add(1, Relaxed);
        }
    }
}

#[derive(Default, Debug)]
pub struct FsStats {
    pub kind: Counter,
    pub read: Counter,
    pub read_dir: Counter,
    pub walk: Counter,
    pub canonical: Counter,
    /// Per-path tallies — the only thing here needing a map, so the only thing behind a
    /// lock, and `None` unless asked for. Gives the distinct-path count (the floor a memo
    /// can reach: if calls ≈ distinct, caching is the wrong fix) and the repeat histogram
    /// that names offenders outright. Too much to carry on every run for a 729-file corpus.
    paths: Option<Mutex<HashMap<PathBuf, usize>>>,
}

impl FsStats {
    /// Path tallying on when `ANSIBLE_LSP_FS_PATHS` is set.
    fn new() -> Self {
        Self {
            paths: std::env::var_os("ANSIBLE_LSP_FS_PATHS")
                .map(|_| Mutex::new(HashMap::new())),
            ..Self::default()
        }
    }

    fn note(&self, p: &Path) {
        if let Some(m) = &self.paths {
            if let Ok(mut g) = m.lock() {
                *g.entry(p.to_path_buf()).or_default() += 1;
            }
        }
    }

    pub fn calls(&self) -> usize {
        self.each().iter().map(|(_, c)| c.calls.load(Relaxed)).sum()
    }

    pub fn misses(&self) -> usize {
        self.each().iter().map(|(_, c)| c.misses.load(Relaxed)).sum()
    }

    pub fn nanos(&self) -> u64 {
        self.each().iter().map(|(_, c)| c.nanos.load(Relaxed)).sum()
    }

    pub fn each(&self) -> [(&'static str, &Counter); 5] {
        [
            ("kind", &self.kind),
            ("read", &self.read),
            ("read_dir", &self.read_dir),
            ("walk", &self.walk),
            ("canonical", &self.canonical),
        ]
    }

    /// Distinct paths touched, and the most-repeated ones. `None` unless path tallying
    /// was enabled.
    pub fn distinct(&self) -> Option<usize> {
        Some(self.paths.as_ref()?.lock().ok()?.len())
    }

    pub fn top_paths(&self, n: usize) -> Vec<(PathBuf, usize)> {
        let Some(m) = &self.paths else { return Vec::new() };
        let Ok(g) = m.lock() else { return Vec::new() };
        let mut v: Vec<(PathBuf, usize)> = g.iter().map(|(p, c)| (p.clone(), *c)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(n);
        v
    }
}

/// Counts what passes through, then delegates. A decorator rather than a field on each
/// [`Fs`], so [`StdFs`] stays a zero-cost unit struct on the interactive paths — and so the
/// *baseline* is measurable with the same code as the optimised side, which is the half
/// nobody takes on faith.
///
/// It stacks: wrapping inside a memo counts syscalls, wrapping outside counts calls, so
/// neither implementation has to track both itself.
pub struct Counting<F: Fs> {
    inner: F,
    stats: FsStats,
}

impl<F: Fs> Counting<F> {
    pub fn new(inner: F) -> Self {
        Self { inner, stats: FsStats::new() }
    }

    pub fn stats(&self) -> &FsStats {
        &self.stats
    }

    pub fn inner(&self) -> &F {
        &self.inner
    }

    /// Tally one delegated call. `missing` feeds the miss counter; only `kind` and
    /// `canonical` can meaningfully answer "nothing there", so the others pass `false`.
    fn timed<T>(&self, c: &Counter, p: &Path, missing: fn(&T) -> bool, f: impl FnOnce() -> T) -> T {
        self.stats.note(p);
        let start = Instant::now();
        let out = f();
        c.record(start.elapsed(), missing(&out));
        out
    }
}

impl<F: Fs> Fs for Counting<F> {
    fn kind(&self, p: &Path) -> Option<Kind> {
        self.timed(&self.stats.kind, p, Option::is_none, || self.inner.kind(p))
    }

    fn symlink_kind(&self, p: &Path) -> Option<Kind> {
        self.timed(&self.stats.kind, p, Option::is_none, || self.inner.symlink_kind(p))
    }

    fn read(&self, p: &Path) -> Option<String> {
        self.timed(&self.stats.read, p, |_| false, || self.inner.read(p))
    }

    fn read_dir(&self, p: &Path) -> Vec<(PathBuf, Kind)> {
        self.timed(&self.stats.read_dir, p, |_| false, || self.inner.read_dir(p))
    }

    fn walk(&self, root: &Path) -> Vec<(PathBuf, Vec<String>)> {
        self.timed(&self.stats.walk, root, |_| false, || self.inner.walk(root))
    }

    fn canonical(&self, p: &Path) -> Option<PathBuf> {
        self.timed(&self.stats.canonical, p, Option::is_none, || self.inner.canonical(p))
    }
}

#[cfg(test)]
mod tests {
    /// A door only works if there is no window (T-085).
    ///
    /// Every filesystem touch must go through [`Fs`], or the counters under-report and the
    /// memo silently misses. That is not hypothetical: T-076's first measurement placed
    /// counters at two call sites, reported 561 probes, and was wrong by 4× — `strace` found
    /// 2340, the rest in `glob`, `include_vars`, `yaml_files` and `canonicalize`. This test
    /// is what makes "one door" a property of the crate rather than a habit.
    #[test]
    fn no_filesystem_call_bypasses_the_seam() {
        // `install.rs` is exempt on purpose: it describes the *machine's* Ansible
        // installation, not workspace state. It is detected once behind a `OnceLock` and
        // caches its own routing tables, so there is no per-scan `Fs` to hand it.
        const EXEMPT: &[&str] = &["fs.rs", "install.rs"];
        const BANNED: &[&str] = &[
            "std::fs::",
            ".is_file()",
            ".is_dir()",
            ".exists()",
            ".canonicalize()",
            ".symlink_metadata(",
            // `.read_dir(` is deliberately absent: `std::fs::read_dir` is already caught
            // by `std::fs::`, and as a bare method it false-positives whenever the
            // receiver `fs` sits on the previous line.
        ];

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                if p.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                if EXEMPT.contains(&name.as_str()) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&p) else { continue };
                // Tests may touch the disk directly — they are building fixtures, not
                // resolving a workspace.
                let body = text.split("\n#[cfg(test)]").next().unwrap_or(&text);
                for (i, line) in body.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or(line);
                    for b in BANNED {
                        if code.contains(b) {
                            found.push(format!("{name}:{}  {}", i + 1, code.trim()));
                        }
                    }
                }
            }
        }
        assert!(
            found.is_empty(),
            "filesystem calls outside the Fs seam — route them through `fs`, or add a \
             documented exemption:\n  {}",
            found.join("\n  ")
        );
    }
}
