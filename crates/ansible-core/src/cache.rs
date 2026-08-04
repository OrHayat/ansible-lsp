//! Scan-scoped memoization for the variable walk (T-076).
//!
//! [`crate::vars::definitions_with_deps`] walks every include / role / meta-dependency edge
//! reachable from one file. Run it per file over a workspace and the shared subtrees — role
//! `defaults`/`vars`/`meta`, a shared task file, `group_vars` — are read, parsed and walked
//! once *per consumer*: O(files x subtree). This cache collapses that to O(files) by
//! remembering, for one scan:
//!
//! 1. each file's text + parse, keyed by canonical path;
//! 2. each directory's [`FileContext`], and each project root's `ansible.cfg`;
//! 3. each file's **raw** contribution to the walk — its defs and the files it read.
//!
//! Layer 3 is the one that matters, and the one with a trap: a walk stamps provenance
//! (`via`, T-066) onto the defs a *caller* pulled in, so what's cached is the contribution
//! with the callee's own chains only, and the caller re-stamps its edge on the clone it
//! merges. Nothing already stamped by an outer frame is ever cached.
//!
//! A cache is scoped to one pass — created by the workspace scan, dropped when it ends — so
//! nothing here can go stale against an edit.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::config::AnsibleConfig;
use crate::fs::{Fs, Kind, StdFs};
use crate::parse::{Document, Node};
use crate::vars::Located;
use crate::workspace::{yaml_files_in, FileContext};

/// A file read from disk once: its text, and its parse (`None` when it isn't valid YAML).
pub struct Source {
    pub text: Arc<str>,
    pub nodes: Option<Arc<Vec<Node>>>,
    /// The canonical path, when the file exists — what a dependency set records.
    pub canon: Option<PathBuf>,
}

/// What one file adds to a variable walk, *before* any caller stamps its provenance on it:
/// the definitions it and its subtree define, and every file that was read to find them.
#[derive(Default)]
pub struct Contribution {
    pub defs: Vec<Located>,
    pub deps: std::collections::HashSet<PathBuf>,
}

/// Work the cache did rather than avoided — the counter the ticket asks for, since on a fast
/// machine the wall clock can't see this phase at all. `edges` is how many times the walk
/// followed an edge into a file; `files` is how many distinct files it actually walked.
#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub edges: usize,
    pub files: usize,
    /// Files walked whose result a cycle truncated, so it couldn't be kept. Included in
    /// `files`; a large share here means the memo isn't earning its keep.
    pub uncached: usize,
    pub reads: usize,
    pub contexts: usize,
    pub configs: usize,
    /// Definitions handed back to callers, summed — the irreducible part, since every file
    /// still materialises its own view of every variable it can see.
    pub defs: usize,
}

#[derive(Default)]
struct Inner {
    canonical: HashMap<PathBuf, Option<PathBuf>>,
    /// Existence probes, **negatives included** — half of them are misses, so a map of
    /// hits alone would leave more than half the work on the floor.
    kinds: HashMap<PathBuf, Option<Kind>>,
    /// Raw directory entries, from which the YAML-filtered `listings` are derived.
    dirs: HashMap<PathBuf, Arc<Vec<(PathBuf, Kind)>>>,
    walks: HashMap<PathBuf, Arc<Vec<(PathBuf, Vec<String>)>>>,
    sources: HashMap<PathBuf, Option<Arc<Source>>>,
    contexts: HashMap<PathBuf, Arc<FileContext>>,
    configs: HashMap<PathBuf, AnsibleConfig>,
    contributions: HashMap<PathBuf, Arc<Contribution>>,
    /// Recursive YAML listings, for `role_task_files`.
    trees: HashMap<PathBuf, Arc<Vec<PathBuf>>>,
    /// One directory's own YAML files, for `group_vars/` and `host_vars/`.
    listings: HashMap<PathBuf, Arc<Vec<PathBuf>>>,
    stats: Stats,
}

/// Shared across the files of one scan. Every method takes `&self` and locks only around the
/// map access, never across the filesystem work — so a miss on one thread doesn't hold the
/// others. Two threads racing the same miss both compute it and agree on the answer.
///
/// One lock for all the maps, deliberately: `stats` is touched by nearly every operation, so
/// per-map locks would make the common path take *two* acquisitions instead of one — a real
/// cost on the ~50–100 ns memo-hit path this exists to create, bought against parallelism the
/// scan loop doesn't use yet. If it ever does, the order is: move `stats` to atomics first
/// (see [`crate::fs::Counter`]), then shard the hot map by path hash. Splitting by field
/// helps least, because every thread hits `contributions` and `canonical` regardless.
pub struct ScanCache {
    /// What answers a miss. `StdFs` normally; wrap it in [`crate::fs::Counting`] to see the
    /// syscalls this saved.
    fs: Box<dyn Fs>,
    inner: Mutex<Inner>,
}

impl Default for ScanCache {
    fn default() -> Self {
        Self::new(StdFs)
    }
}

impl ScanCache {
    pub fn new(fs: impl Fs + 'static) -> Self {
        Self { fs: Box::new(fs), inner: Mutex::new(Inner::default()) }
    }

    /// The backend a miss falls through to — where its counters live, if it has any.
    pub fn backend(&self) -> &dyn Fs {
        &*self.fs
    }

    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> Option<T> {
        self.inner.lock().ok().map(|mut i| f(&mut i))
    }

    pub fn stats(&self) -> Stats {
        self.with(|i| i.stats).unwrap_or_default()
    }

    /// Resolve the parent — memoized, so a shared prefix is walked once for the whole scan —
    /// then settle the final component with a single `lstat`.
    ///
    /// `realpath` interrogates *every* component, so the naive version spends 711 of 712
    /// `readlink` calls learning that `/mnt`, `/mnt/c`, `/mnt/c/Users` … are still not
    /// symlinks, once per file. Resolving per directory turns a new file in a known
    /// directory into one syscall.
    ///
    /// Not on Windows: `canonicalize` there is a single handle open rather than a component
    /// walk, so there is little to save — and it returns the *on-disk* casing, which this
    /// shortcut cannot (it keeps the caller's). On a case-insensitive filesystem that would
    /// hand two spellings of one file two identities, and identity is what cycle detection
    /// and the dependency sets are built on.
    fn canonical_uncached(&self, path: &Path) -> Option<PathBuf> {
        if cfg!(windows) {
            return self.fs.canonical(path);
        }
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return self.fs.canonical(path);
        };
        // A `.`/`..` tail has no name to append — let the real thing sort it out.
        if parent.as_os_str().is_empty()
            || !matches!(
                path.components().next_back(),
                Some(std::path::Component::Normal(_))
            )
        {
            return self.fs.canonical(path);
        }
        let candidate = Fs::canonical(self, parent)?.join(name);
        match self.fs.symlink_kind(&candidate) {
            None => None,
            // A symlink (or something stranger) at the tail: resolve it properly.
            Some(Kind::Other) => self.fs.canonical(&candidate),
            Some(_) => Some(candidate),
        }
    }

    /// Read and parse `path` once per scan. `None` when it can't be read.
    pub fn source(&self, path: &Path) -> Option<Arc<Source>> {
        let canon = Fs::canonical(self, path);
        let key = canon.clone().unwrap_or_else(|| path.to_path_buf());
        // Two levels of `Option` collapse here: "the lock is gone" and "never looked at"
        // both mean recompute, while a remembered *failure* is a hit that returns `None`.
        if let Some(hit) = self.with(|i| i.sources.get(&key).cloned()).flatten() {
            return hit;
        }
        let src = self.fs.read(path).map(|text| {
            let doc = Document::new(text);
            let nodes = doc.parse().map(Arc::new);
            Arc::new(Source {
                text: Arc::from(doc.text.as_str()),
                nodes,
                canon: canon.clone(),
            })
        });
        self.with(|i| {
            i.stats.reads += 1;
            i.sources.insert(key, src.clone());
        });
        src
    }

    /// Record a file the caller already read and parsed — the scan reads each file once as
    /// the subject of its own analysis, and would otherwise read it again when some other
    /// file's walk reaches it.
    pub fn prime(&self, path: &Path, text: &str, nodes: &[Node]) {
        let Some(canon) = Fs::canonical(self, path) else { return };
        self.with(|i| {
            i.sources.entry(canon.clone()).or_insert_with(|| {
                Some(Arc::new(Source {
                    text: Arc::from(text),
                    nodes: Some(Arc::new(nodes.to_vec())),
                    canon: Some(canon),
                }))
            });
        });
    }

    /// [`FileContext::discover`] for `file`, memoized by its directory — which is all
    /// `discover` looks at — with the project's `ansible.cfg` read at most once.
    pub fn context(&self, file: &Path) -> Arc<FileContext> {
        let dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
        if let Some(hit) = self.with(|i| i.contexts.get(&dir).cloned()).flatten() {
            return hit;
        }
        let ctx = Arc::new(FileContext::discover_with(file, self, |root| self.config(root)));
        self.with(|i| {
            i.stats.contexts += 1;
            i.contexts.insert(dir, ctx.clone());
        });
        ctx
    }

    fn config(&self, root: &Path) -> AnsibleConfig {
        if let Some(hit) = self.with(|i| i.configs.get(root).cloned()).flatten() {
            return hit;
        }
        let cfg = AnsibleConfig::load_in(root, self);
        self.with(|i| {
            i.stats.configs += 1;
            i.configs.insert(root.to_path_buf(), cfg.clone());
        });
        cfg
    }

    /// Every YAML file under `dir`, memoized.
    pub fn tree(&self, dir: &Path) -> Arc<Vec<PathBuf>> {
        if let Some(hit) = self.with(|i| i.trees.get(dir).cloned()).flatten() {
            return hit;
        }
        let files = Arc::new(yaml_files_in(dir, self));
        self.with(|i| i.trees.insert(dir.to_path_buf(), files.clone()));
        files
    }

    /// `dir`'s own `*.yml`/`*.yaml` files, not descending — memoized. Empty when `dir`
    /// isn't a directory.
    pub fn listing(&self, dir: &Path) -> Arc<Vec<PathBuf>> {
        if let Some(hit) = self.with(|i| i.listings.get(dir).cloned()).flatten() {
            return hit;
        }
        let files = Arc::new(
            Fs::read_dir(self, dir)
                .into_iter()
                .map(|(p, _)| p)
                .filter(|p| {
                    matches!(
                        p.extension().and_then(|s| s.to_str()),
                        Some("yml") | Some("yaml")
                    )
                })
                .collect::<Vec<_>>(),
        );
        self.with(|i| i.listings.insert(dir.to_path_buf(), files.clone()));
        files
    }

    /// The memoized contribution of `canon`, and a tick on the edge counter.
    pub fn contribution(&self, canon: &Path) -> Option<Arc<Contribution>> {
        self.with(|i| {
            i.stats.edges += 1;
            i.contributions.get(canon).cloned()
        })
        .flatten()
    }

    pub fn store(&self, canon: PathBuf, c: Arc<Contribution>) {
        self.with(|i| {
            i.stats.files += 1;
            i.contributions.insert(canon, c);
        });
    }

    /// A file walked but deliberately not memoized (its result was truncated by a cycle).
    pub fn count_uncached(&self) {
        self.with(|i| {
            i.stats.files += 1;
            i.stats.uncached += 1;
        });
    }

    pub fn count_defs(&self, n: usize) {
        self.with(|i| i.stats.defs += n);
    }
}

/// The memoizing half of the seam (T-085). Every answer is remembered for the pass —
/// **including "nothing there"**, which is half of all probes in a real scan: role search
/// re-testing the same missing name against the same roots, the `ansible.cfg` walk-up
/// re-testing the same ancestors.
impl Fs for ScanCache {
    fn kind(&self, p: &Path) -> Option<Kind> {
        if let Some(hit) = self.with(|i| i.kinds.get(p).copied()).flatten() {
            return hit;
        }
        let k = self.fs.kind(p);
        self.with(|i| i.kinds.insert(p.to_path_buf(), k));
        k
    }

    /// Not memoized: the caller only asks about a path whose parent it just resolved, so
    /// there is no repeat to save, and caching `lstat` alongside `stat` would double the
    /// map for one use.
    fn symlink_kind(&self, p: &Path) -> Option<Kind> {
        self.fs.symlink_kind(p)
    }

    /// Shares [`ScanCache::source`]'s read, so a file that is both parsed by the walk and
    /// read as a vars file crosses the filesystem once.
    fn read(&self, p: &Path) -> Option<String> {
        Some(self.source(p)?.text.to_string())
    }

    /// Seeds the existence map as a side effect: the listing already knows what each entry
    /// is, so every later `kind()` on one of them is answered without a syscall.
    fn read_dir(&self, p: &Path) -> Vec<(PathBuf, Kind)> {
        if let Some(hit) = self.with(|i| i.dirs.get(p).cloned()).flatten() {
            return (*hit).clone();
        }
        let entries = Arc::new(self.fs.read_dir(p));
        self.with(|i| {
            for (path, kind) in entries.iter() {
                i.kinds.entry(path.clone()).or_insert(Some(*kind));
            }
            i.dirs.insert(p.to_path_buf(), entries.clone());
        });
        (*entries).clone()
    }

    fn walk(&self, root: &Path) -> Vec<(PathBuf, Vec<String>)> {
        if let Some(hit) = self.with(|i| i.walks.get(root).cloned()).flatten() {
            return (*hit).clone();
        }
        let out = Arc::new(self.fs.walk(root));
        self.with(|i| i.walks.insert(root.to_path_buf(), out.clone()));
        (*out).clone()
    }

    fn canonical(&self, p: &Path) -> Option<PathBuf> {
        if let Some(hit) = self.with(|i| i.canonical.get(p).cloned()).flatten() {
            return hit;
        }
        let out = self.canonical_uncached(p);
        self.with(|i| i.canonical.insert(p.to_path_buf(), out.clone()));
        out
    }
}
