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

    /// Is this file executable by anyone? The one question that separates a dynamic
    /// inventory — a script Ansible *runs* — from a static one it reads (T-062), and the
    /// reason we never run it.
    ///
    /// Defaults to `false`, which is right for in-memory trees (nothing there is a real
    /// program) and on Windows, where the bit does not exist. Only a real filesystem
    /// overrides it, so a fake can still assert the plugin-config half of the detection.
    fn is_executable(&self, _p: &Path) -> bool {
        false
    }

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

    /// Any execute bit. Unix only — on Windows the concept does not exist and the trait
    /// default (`false`) stands, which costs nothing: an inventory that is a script is a
    /// Unix arrangement, and misreading one as static text is a miss, never a wrong claim.
    #[cfg(unix)]
    fn is_executable(&self, p: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
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
        // `install.rs` is exempt on purpose: detection describes the *machine's* Ansible
        // installation, not workspace state, and it runs once on a blocking task before any
        // scan exists — there is no `Fs` to hand it.
        //
        // That justification used to be stretched to cover reading a *collection's*
        // `meta/runtime.yml`, and every clause of it was false there: such a table can live at
        // `<project_root>/collections/ansible_collections/…`, so it is workspace state the
        // user edits; the caller already held an `Fs` and used it for `is_file` on the line
        // before; and the "cached behind a `OnceLock`" clause described globals T-201 removed.
        // The cost was measured, not theorised: an edited routing table did nothing until the
        // server restarted. Collection tables now go through `cache::RoutingTables`, which
        // reads via this seam.
        //
        // `testing.rs` builds fixture trees on disk, which is the same exemption the
        // `#[cfg(test)]` split below grants every other file's tests — it needs naming here
        // only because its `cfg(test)` sits on the `mod` in `lib.rs`, so the file has no
        // in-body marker to split on. It is never compiled into a release build.
        const EXEMPT: &[&str] = &["fs.rs", "install.rs", "testing.rs"];
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

    use super::*;

    fn tree(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("a.yml"), "x: 1\n").unwrap();
        std::fs::write(d.join("sub/b.yml"), "y: 2\n").unwrap();
        d
    }

    /// [`StdFs`] against a real tree: the four kind answers, and the two that only a real
    /// filesystem can produce.
    #[test]
    fn stdfs_reports_each_kind_it_can_meet() {
        let d = tree("ansible-lsp-fs-kinds");
        let fs = StdFs;

        assert_eq!(fs.kind(&d.join("a.yml")), Some(Kind::File));
        assert_eq!(fs.kind(&d.join("sub")), Some(Kind::Dir));
        assert_eq!(fs.kind(&d.join("nope")), None, "nothing there is None, not Other");
        assert!(fs.is_file(&d.join("a.yml")));
        assert!(fs.is_dir(&d.join("sub")));
        assert!(fs.exists(&d.join("a.yml")));
        assert!(!fs.exists(&d.join("nope")));

        assert_eq!(fs.read(&d.join("a.yml")).as_deref(), Some("x: 1\n"));
        assert_eq!(fs.read(&d.join("nope")), None, "an unreadable path is None, never empty");
        // A directory is not readable as text, and must not come back as an empty file.
        assert_eq!(fs.read(&d.join("sub")), None);
    }

    /// `symlink_kind` is the one method whose whole purpose is *not* matching `kind`.
    ///
    /// A symlink reports `Other` however it resolves — the signal to resolve it properly —
    /// while `kind` follows it. A test asserting only one of them would pass against an
    /// implementation that had them identical, which is exactly the default this overrides.
    #[cfg(unix)]
    #[test]
    fn symlink_kind_refuses_to_follow_where_kind_follows() {
        let d = tree("ansible-lsp-fs-links");
        let link = d.join("link_to_a");
        std::os::unix::fs::symlink(d.join("a.yml"), &link).unwrap();
        let dirlink = d.join("link_to_sub");
        std::os::unix::fs::symlink(d.join("sub"), &dirlink).unwrap();
        let fs = StdFs;

        assert_eq!(fs.kind(&link), Some(Kind::File), "kind follows the link");
        assert_eq!(fs.symlink_kind(&link), Some(Kind::Other), "symlink_kind does not");
        assert_eq!(fs.kind(&dirlink), Some(Kind::Dir));
        assert_eq!(fs.symlink_kind(&dirlink), Some(Kind::Other));
        // A link to nothing: both agree it is not there, by different routes.
        let broken = d.join("broken");
        std::os::unix::fs::symlink(d.join("gone"), &broken).unwrap();
        assert_eq!(fs.kind(&broken), None, "kind follows into nothing");
        assert_eq!(fs.symlink_kind(&broken), Some(Kind::Other), "the link itself is there");

        // A symlinked directory still lists as a Dir, which is what keeps a symlinked role
        // walkable — `file_type()` alone would call it Other.
        let listed = fs.read_dir(&d);
        let seen = listed.iter().find(|(p, _)| p == &dirlink).expect("the link is listed");
        assert_eq!(seen.1, Kind::Dir, "read_dir resolves a symlinked directory");
    }

    /// `read_dir`, `read_dir_paths` and `walk` describe the same tree, so they must agree.
    #[test]
    fn the_listing_walk_and_paths_view_agree() {
        let d = tree("ansible-lsp-fs-walk");
        let fs = StdFs;

        let mut names: Vec<String> = fs
            .read_dir(&d)
            .into_iter()
            .filter_map(|(p, _)| p.file_name()?.to_str().map(str::to_owned))
            .collect();
        names.sort();
        assert_eq!(names, ["a.yml", "sub"]);

        // The default `read_dir_paths` is the same listing minus the kinds.
        let mut paths = fs.read_dir_paths(&d);
        paths.sort();
        let mut expect = vec![d.join("a.yml"), d.join("sub")];
        expect.sort();
        assert_eq!(paths, expect);

        // `walk` yields one entry per directory, files only — subdirectories appear as their
        // own entry rather than as a name in the parent's list.
        let walked = fs.walk(&d);
        let of = |dir: &Path| {
            walked.iter().find(|(p, _)| p == dir).map(|(_, f)| {
                let mut v = f.clone();
                v.sort();
                v
            })
        };
        assert_eq!(of(&d).unwrap(), ["a.yml"], "sub is a directory, not a file name");
        assert_eq!(of(&d.join("sub")).unwrap(), ["b.yml"]);

        // A path that is not a directory lists nothing rather than failing.
        assert!(fs.read_dir(&d.join("a.yml")).is_empty());
        assert!(fs.read_dir(&d.join("nope")).is_empty());
    }

    #[test]
    fn canonical_and_same_file_see_through_a_relative_spelling() {
        let d = tree("ansible-lsp-fs-canon");
        let fs = StdFs;
        let direct = d.join("a.yml");
        let roundabout = d.join("sub").join("..").join("a.yml");

        assert_eq!(fs.canonical(&direct), fs.canonical(&roundabout));
        assert!(fs.same_file(&direct, &roundabout), "two spellings, one file");
        assert!(!fs.same_file(&direct, &d.join("sub/b.yml")), "control: different files");
        // Nothing there canonicalises to nothing, and `same_file` is false rather than a
        // vacuous true when either side is missing.
        assert_eq!(fs.canonical(&d.join("nope")), None);
        assert!(!fs.same_file(&d.join("nope"), &d.join("nope")));
    }

    #[cfg(unix)]
    #[test]
    fn is_executable_reads_any_execute_bit() {
        use std::os::unix::fs::PermissionsExt;
        let d = tree("ansible-lsp-fs-exec");
        let f = d.join("a.yml");
        let fs = StdFs;
        let chmod = |m: u32| {
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(m)).unwrap();
        };
        chmod(0o644);
        assert!(!fs.is_executable(&f), "control: a plain file is not executable");
        chmod(0o755);
        assert!(fs.is_executable(&f), "owner bit");
        chmod(0o604);
        assert!(!fs.is_executable(&f));
        chmod(0o614);
        assert!(fs.is_executable(&f), "group bit alone counts");
        chmod(0o645);
        assert!(fs.is_executable(&f), "other bit alone counts");
        chmod(0o644);
        assert!(!fs.is_executable(&d.join("nope")), "a missing path is not executable");
    }

    /// The [`Counting`] decorator: every method tallied to its own counter, misses separated
    /// from hits, and the totals summing what the five counters hold.
    #[test]
    fn counting_tallies_each_method_separately() {
        let d = tree("ansible-lsp-fs-count");
        let fs = Counting::new(StdFs);

        fs.kind(&d.join("a.yml"));
        fs.kind(&d.join("nope"));
        fs.symlink_kind(&d.join("a.yml"));
        fs.read(&d.join("a.yml"));
        fs.read_dir(&d);
        fs.walk(&d);
        fs.canonical(&d.join("a.yml"));
        fs.canonical(&d.join("nope"));

        let s = fs.stats();
        // `symlink_kind` shares the `kind` counter deliberately — both are one stat call.
        assert_eq!(s.kind.calls.load(Relaxed), 3);
        assert_eq!(s.read.calls.load(Relaxed), 1);
        assert_eq!(s.read_dir.calls.load(Relaxed), 1);
        assert_eq!(s.walk.calls.load(Relaxed), 1);
        assert_eq!(s.canonical.calls.load(Relaxed), 2);
        assert_eq!(s.calls(), 8, "the total is the sum of the five");

        // Only `kind` and `canonical` can answer "nothing there", so only they miss.
        assert_eq!(s.kind.misses.load(Relaxed), 1);
        assert_eq!(s.canonical.misses.load(Relaxed), 1);
        assert_eq!(s.read.misses.load(Relaxed), 0, "a read never reports a miss");
        assert_eq!(s.misses(), 2);

        // `each` names all five, in a fixed order the report depends on.
        let names: Vec<&str> = s.each().iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["kind", "read", "read_dir", "walk", "canonical"]);
        assert_eq!(s.each().len(), 5);

        // The decorator still answers correctly — counting must not change the answer.
        assert_eq!(fs.inner().kind(&d.join("a.yml")), Some(Kind::File));
        assert_eq!(fs.read(&d.join("a.yml")).as_deref(), Some("x: 1\n"));
    }

    /// Path tallying is off unless asked for, because it is the one thing here behind a lock.
    #[test]
    fn path_tallies_are_absent_until_the_env_asks_for_them() {
        let d = tree("ansible-lsp-fs-paths");
        let fs = Counting::new(StdFs);
        fs.kind(&d.join("a.yml"));
        assert_eq!(fs.stats().distinct(), None, "off by default");
        assert!(fs.stats().top_paths(5).is_empty());
    }

    /// Stacking is the whole design: an inner [`Counting`] counts what reached the disk, an
    /// outer one counts what was asked. Neither implementation tracks both.
    #[test]
    fn counting_wrappers_stack_and_arc_delegates_every_method() {
        let d = tree("ansible-lsp-fs-stack");
        let inner = std::sync::Arc::new(Counting::new(StdFs));
        let outer = Counting::new(inner.clone());

        // Through the Arc impl, which delegates each method to the inner value.
        outer.kind(&d.join("a.yml"));
        outer.symlink_kind(&d.join("a.yml"));
        outer.read(&d.join("a.yml"));
        outer.read_dir(&d);
        outer.walk(&d);
        outer.canonical(&d.join("a.yml"));

        assert_eq!(outer.stats().calls(), 6, "the outer layer saw every ask");
        assert_eq!(inner.stats().calls(), 6, "and each one reached the disk");

        // The Arc's own trait impl answers the same as the thing it wraps.
        let arc: std::sync::Arc<Counting<StdFs>> = inner.clone();
        assert_eq!(arc.kind(&d.join("a.yml")), Some(Kind::File));
        assert_eq!(arc.symlink_kind(&d.join("sub")), Some(Kind::Dir));
        assert_eq!(arc.read(&d.join("a.yml")).as_deref(), Some("x: 1\n"));
        assert_eq!(arc.read_dir(&d).len(), 2);
        assert!(!arc.walk(&d).is_empty());
        assert!(arc.canonical(&d.join("a.yml")).is_some());
    }
}
