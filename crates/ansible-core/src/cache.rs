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
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};

use crate::config::{AnsibleConfig, EnvMap};
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

/// Atomics, not fields under the map lock: nearly every operation ticks one, so sharing the
/// lock serialised operations whose maps could never collide.
#[derive(Default)]
struct AtomicStats {
    edges: AtomicUsize,
    files: AtomicUsize,
    uncached: AtomicUsize,
    reads: AtomicUsize,
    contexts: AtomicUsize,
    configs: AtomicUsize,
    defs: AtomicUsize,
}

impl AtomicStats {
    fn snapshot(&self) -> Stats {
        Stats {
            edges: self.edges.load(Ordering::Relaxed),
            files: self.files.load(Ordering::Relaxed),
            uncached: self.uncached.load(Ordering::Relaxed),
            reads: self.reads.load(Ordering::Relaxed),
            contexts: self.contexts.load(Ordering::Relaxed),
            configs: self.configs.load(Ordering::Relaxed),
            defs: self.defs.load(Ordering::Relaxed),
        }
    }
}

const SHARDS: usize = 32;

/// A path-keyed map split into [`SHARDS`] independently locked pieces, so two threads
/// touching unrelated paths don't queue behind each other. Sharded rather than one
/// `RwLock`: the miss path *writes*, and a scan's first pass is nearly all misses.
struct Map<V> {
    shards: [Mutex<HashMap<PathBuf, V>>; SHARDS],
}

impl<V> Default for Map<V> {
    fn default() -> Self {
        Self { shards: std::array::from_fn(|_| Mutex::new(HashMap::new())) }
    }
}

impl<V: Clone> Map<V> {
    fn slot(&self, k: &Path) -> &Mutex<HashMap<PathBuf, V>> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        k.hash(&mut h);
        &self.shards[h.finish() as usize % SHARDS]
    }

    fn get(&self, k: &Path) -> Option<V> {
        self.slot(k).lock().ok()?.get(k).cloned()
    }

    fn insert(&self, k: PathBuf, v: V) {
        if let Ok(mut m) = self.slot(&k).lock() {
            m.insert(k, v);
        }
    }

    fn or_insert_with(&self, k: PathBuf, f: impl FnOnce() -> V) {
        if let Ok(mut m) = self.slot(&k).lock() {
            m.entry(k).or_insert_with(f);
        }
    }
}

/// A [`Map`] that computes each key **once**, even under a cold-cache stampede.
///
/// `Map`'s get-then-insert lets every thread that misses the same cold key run the compute;
/// they agree on the answer, so it is correct, and on a local filesystem it is also cheap.
/// It is not cheap when the compute is a `9p` round trip: 32 threads starting together all
/// miss, and one `ansible.cfg` gets read 15 times (measured on the demo).
///
/// The cell, not the shard lock, is what they wait on — so a thread wanting a *different*
/// key is never blocked behind someone else's filesystem call. The shard lock is held only
/// long enough to hand out the cell.
///
/// Not for a memo whose compute can re-enter the same key: the second entry would wait on a
/// cell only it can fill. `contributions` is exactly that (a walk reaching a cycle comes back
/// to its own key), which is why it stays a plain `Map`.
struct Flight<V> {
    cells: Map<Arc<OnceLock<V>>>,
}

impl<V> Default for Flight<V> {
    fn default() -> Self {
        Self { cells: Map::default() }
    }
}

impl<V: Clone> Flight<V> {
    /// The value for `k`, and whether *this* caller computed it — the flag is what the stats
    /// count, so they report distinct computations rather than attempts.
    fn get_or_init(&self, k: &Path, f: impl FnOnce() -> V) -> (V, bool) {
        let cell = {
            let mut shard = match self.cells.slot(k).lock() {
                Ok(s) => s,
                // Poisoned: compute without memoizing rather than fail the scan.
                Err(_) => return (f(), true),
            };
            shard.entry(k.to_path_buf()).or_default().clone()
        };
        let mut computed = false;
        let v = cell.get_or_init(|| {
            computed = true;
            f()
        });
        (v.clone(), computed)
    }

    /// Fill the cell if it is still empty, with a value the caller already has.
    fn prime(&self, k: &Path, v: impl FnOnce() -> V) {
        let cell = {
            let Ok(mut shard) = self.cells.slot(k).lock() else { return };
            shard.entry(k.to_path_buf()).or_default().clone()
        };
        let _ = cell.get_or_init(v);
    }
}

/// One directory the walk visited: its path, and the names of the YAML files directly in it
/// — the shape [`crate::fs::Fs::walk`] returns, named so the memo's type reads.
type Walk = (PathBuf, Vec<String>);

/// Shared across the files of one scan. Every method takes `&self` and locks only around the
/// map access, never across the filesystem work — so a miss on one thread doesn't hold the
/// others. Two threads racing the same miss both compute it and agree on the answer.
///
/// Sharded by path hash rather than one lock over everything: the scan walks files
/// concurrently, and a memo hit is ~50–100 ns, so on a local filesystem contention cost
/// more than the syscalls the memo saves.
pub struct ScanCache {
    /// What answers a miss. `StdFs` normally; wrap it in [`crate::fs::Counting`] to see the
    /// syscalls this saved.
    fs: Box<dyn Fs>,
    canonical: Map<Option<PathBuf>>,
    /// Existence probes, **negatives included** — half of them are misses, so a map of
    /// hits alone would leave more than half the work on the floor.
    kinds: Map<Option<Kind>>,
    /// Raw directory entries, from which the YAML-filtered `listings` are derived.
    dirs: Map<Arc<Vec<(PathBuf, Kind)>>>,
    walks: Map<Arc<Vec<Walk>>>,
    sources: Flight<Option<Arc<Source>>>,
    contexts: Flight<Arc<FileContext>>,
    configs: Flight<AnsibleConfig>,
    contributions: Map<Arc<Contribution>>,
    /// Recursive YAML listings, for `role_task_files`.
    trees: Map<Arc<Vec<PathBuf>>>,
    /// One directory's own YAML files, for `group_vars/` and `host_vars/`.
    listings: Map<Arc<Vec<PathBuf>>>,
    /// What config loads read — the process snapshot normally; [`with_env`](Self::with_env)
    /// swaps it so a test's fixtures can't be hijacked by the developer's shell.
    env: EnvMap,
    /// The editor's `ansibleLsp.inventory`, standing in for `-i` (T-062). Empty means
    /// "model a plain `ansible-playbook`" and the config's own resolution stands. Held
    /// here rather than threaded through the walk because it is the top rung of the same
    /// ladder `config()` already resolves.
    inventory_override: Vec<PathBuf>,
    stats: AtomicStats,
}

impl Default for ScanCache {
    fn default() -> Self {
        Self::new(StdFs)
    }
}

impl ScanCache {
    pub fn new(fs: impl Fs + 'static) -> Self {
        Self {
            fs: Box::new(fs),
            canonical: Map::default(),
            kinds: Map::default(),
            dirs: Map::default(),
            walks: Map::default(),
            sources: Flight::default(),
            contexts: Flight::default(),
            configs: Flight::default(),
            contributions: Map::default(),
            trees: Map::default(),
            listings: Map::default(),
            env: EnvMap::from_process(),
            inventory_override: Vec::new(),
            stats: AtomicStats::default(),
        }
    }

    /// Point the walk at the inventory the user says they run with — `ansibleLsp.inventory`,
    /// standing in for `-i` (T-062). Empty restores the plain-`ansible-playbook` model.
    /// Must be set before any config is computed, since configs are memoized per root.
    pub fn with_inventory(mut self, paths: Vec<PathBuf>) -> Self {
        self.inventory_override = paths;
        self
    }

    /// Replace the environment config loads see. For tests: `.with_env(EnvMap::empty())`
    /// keeps a fixture project's `ansible.cfg` from being overridden by whatever
    /// `ANSIBLE_CONFIG`/`ANSIBLE_ROLES_PATH` the invoking shell happens to export.
    pub fn with_env(mut self, env: EnvMap) -> Self {
        self.env = env;
        self
    }

    /// The backend a miss falls through to — where its counters live, if it has any.
    pub fn backend(&self) -> &dyn Fs {
        &*self.fs
    }

    pub fn stats(&self) -> Stats {
        self.stats.snapshot()
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
        let (src, computed) = self.sources.get_or_init(&key, || {
            self.fs.read(path).map(|text| {
                let doc = Document::new(text);
                let nodes = doc.parse().map(Arc::new);
                Arc::new(Source {
                    text: Arc::from(doc.text.as_str()),
                    nodes,
                    canon: canon.clone(),
                })
            })
        });
        if computed {
            self.stats.reads.fetch_add(1, Ordering::Relaxed);
        }
        src
    }

    /// Record a file the caller already read and parsed — the scan reads each file once as
    /// the subject of its own analysis, and would otherwise read it again when some other
    /// file's walk reaches it.
    pub fn prime(&self, path: &Path, text: &str, nodes: &[Node]) {
        let Some(canon) = Fs::canonical(self, path) else { return };
        self.sources.prime(&canon.clone(), || {
            Some(Arc::new(Source {
                text: Arc::from(text),
                nodes: Some(Arc::new(nodes.to_vec())),
                canon: Some(canon),
            }))
        });
    }

    /// [`FileContext::discover`] for `file`, memoized by its directory — which is all
    /// `discover` looks at — with the project's `ansible.cfg` read at most once.
    pub fn context(&self, file: &Path) -> Arc<FileContext> {
        let dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
        let (ctx, computed) = self.contexts.get_or_init(&dir, || {
            Arc::new(FileContext::discover_with(file, self, |root| self.config(root)))
        });
        if computed {
            self.stats.contexts.fetch_add(1, Ordering::Relaxed);
        }
        ctx
    }

    fn config(&self, root: &Path) -> AnsibleConfig {
        let (cfg, computed) = self.configs.get_or_init(root, || {
            let mut cfg = AnsibleConfig::builder(root).fs(self).env(&self.env).load();
            // The editor's `ansibleLsp.inventory` is the top rung of the same ladder the
            // config already resolved — it stands in for `-i`, which beats the env var and
            // the file both (measured). Applied here so the whole walk sees one settled
            // answer and `inventory::sources` needs no override parameter.
            if !self.inventory_override.is_empty() {
                cfg.inventory = Some(self.inventory_override.clone());
            }
            cfg
        });
        if computed {
            self.stats.configs.fetch_add(1, Ordering::Relaxed);
        }
        cfg
    }

    /// Every YAML file under `dir`, memoized.
    pub fn tree(&self, dir: &Path) -> Arc<Vec<PathBuf>> {
        if let Some(hit) = self.trees.get(dir) {
            return hit;
        }
        let files = Arc::new(yaml_files_in(dir, self));
        self.trees.insert(dir.to_path_buf(), files.clone());
        files
    }

    /// One `group_vars/`/`host_vars/` directory's entries, not descending — memoized, and
    /// empty when `dir` isn't a directory.
    ///
    /// Ansible probes `''` before `.yml`/`.yaml`/`.json` and breaks on the first hit
    /// (`parsing/dataloader.py:470-491`), which has two consequences this encodes (T-062):
    ///
    /// - an **extension-less** `group_vars/webservers` is a legal, common vars file, so the
    ///   old `*.yml`-only filter dropped a real source;
    /// - a `group_vars/webservers/` **directory silently shadows** `group_vars/webservers.yml`
    ///   — measured, with both present the directory's value is the one that reaches the
    ///   play. That is the opposite of role `defaults/`, where the file wins
    ///   (`role/__init__.py:426-431`), so it cannot be inferred from the neighbouring rule.
    ///
    /// Entries are sorted, so a shadowed file is dropped deterministically rather than by
    /// whatever order the filesystem returned.
    pub fn listing(&self, dir: &Path) -> Arc<Vec<PathBuf>> {
        if let Some(hit) = self.listings.get(dir) {
            return hit;
        }
        let mut entries: Vec<(PathBuf, crate::fs::Kind)> = Fs::read_dir(self, dir);
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        // A directory here is a vars *source*, and its bare name is what it shadows.
        // Ordered, not a HashSet: these are chained into the returned list, and `vars::dedup`
        // keeps the FIRST occurrence of a name — so hash order would decide which definition
        // wins and the scan would answer differently run to run. It did: 233 then 246
        // undefined variables across two runs of the same tree.
        let dirs: std::collections::BTreeSet<PathBuf> = entries
            .iter()
            .filter(|(_, k)| *k == crate::fs::Kind::Dir)
            .map(|(p, _)| p.clone())
            .collect();
        let files = Arc::new(
            entries
                .iter()
                .filter(|(p, k)| match k {
                    // Every file in a shadowing directory is read, in name order.
                    crate::fs::Kind::Dir => false,
                    _ => {
                        let ext = p.extension().and_then(|s| s.to_str());
                        let named = matches!(ext, None | Some("yml") | Some("yaml"));
                        // `webservers.yml` beside a `webservers/` directory never loads.
                        named && !dirs.contains(&p.with_extension(""))
                    }
                })
                .map(|(p, _)| p.clone())
                .chain(dirs.iter().flat_map(|d| {
                    let mut inner: Vec<PathBuf> = Fs::read_dir(self, d)
                        .into_iter()
                        .filter(|(_, k)| *k == crate::fs::Kind::File)
                        .map(|(p, _)| p)
                        .collect();
                    inner.sort();
                    inner
                }))
                .collect::<Vec<_>>(),
        );
        self.listings.insert(dir.to_path_buf(), files.clone());
        files
    }

    /// The memoized contribution of `canon`, and a tick on the edge counter.
    pub fn contribution(&self, canon: &Path) -> Option<Arc<Contribution>> {
        self.stats.edges.fetch_add(1, Ordering::Relaxed);
        self.contributions.get(canon)
    }

    pub fn store(&self, canon: PathBuf, c: Arc<Contribution>) {
        self.stats.files.fetch_add(1, Ordering::Relaxed);
        self.contributions.insert(canon, c);
    }

    /// A file walked but deliberately not memoized (its result was truncated by a cycle).
    pub fn count_uncached(&self) {
        self.stats.files.fetch_add(1, Ordering::Relaxed);
        self.stats.uncached.fetch_add(1, Ordering::Relaxed);
    }

    pub fn count_defs(&self, n: usize) {
        self.stats.defs.fetch_add(n, Ordering::Relaxed);
    }
}

/// The memoizing half of the seam (T-085). Every answer is remembered for the pass —
/// **including "nothing there"**, which is half of all probes in a real scan: role search
/// re-testing the same missing name against the same roots, the `ansible.cfg` walk-up
/// re-testing the same ancestors.
impl Fs for ScanCache {
    fn kind(&self, p: &Path) -> Option<Kind> {
        if let Some(hit) = self.kinds.get(p) {
            return hit;
        }
        let k = self.fs.kind(p);
        self.kinds.insert(p.to_path_buf(), k);
        k
    }

    /// Not memoized: the caller only asks about a path whose parent it just resolved, so
    /// there is no repeat to save, and caching `lstat` alongside `stat` would double the
    /// map for one use.
    fn symlink_kind(&self, p: &Path) -> Option<Kind> {
        self.fs.symlink_kind(p)
    }

    /// Delegated, and load-bearing: the trait default is `false`, so without this the
    /// dynamic-inventory check (T-062) is dead code on every real path — a walk reaches
    /// the filesystem only through this cache. Asked once per inventory source, so there
    /// is nothing to memoize.
    fn is_executable(&self, p: &Path) -> bool {
        self.fs.is_executable(p)
    }

    /// Shares [`ScanCache::source`]'s read, so a file that is both parsed by the walk and
    /// read as a vars file crosses the filesystem once.
    fn read(&self, p: &Path) -> Option<String> {
        Some(self.source(p)?.text.to_string())
    }

    /// Seeds the existence map as a side effect: the listing already knows what each entry
    /// is, so every later `kind()` on one of them is answered without a syscall.
    fn read_dir(&self, p: &Path) -> Vec<(PathBuf, Kind)> {
        if let Some(hit) = self.dirs.get(p) {
            return (*hit).clone();
        }
        let entries = Arc::new(self.fs.read_dir(p));
        for (path, kind) in entries.iter() {
            self.kinds.or_insert_with(path.clone(), || Some(*kind));
        }
        self.dirs.insert(p.to_path_buf(), entries.clone());
        (*entries).clone()
    }

    fn walk(&self, root: &Path) -> Vec<Walk> {
        if let Some(hit) = self.walks.get(root) {
            return (*hit).clone();
        }
        let out = Arc::new(self.fs.walk(root));
        self.walks.insert(root.to_path_buf(), out.clone());
        (*out).clone()
    }

    fn canonical(&self, p: &Path) -> Option<PathBuf> {
        if let Some(hit) = self.canonical.get(p) {
            return hit;
        }
        let out = self.canonical_uncached(p);
        self.canonical.insert(p.to_path_buf(), out.clone());
        out
    }
}
