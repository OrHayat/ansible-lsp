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
#[derive(Default, Clone)]
pub struct Contribution {
    pub defs: Vec<Located>,
    pub deps: std::collections::HashSet<PathBuf>,
    /// Host names a literal `add_host` creates anywhere in the reachable set (T-179).
    ///
    /// Carried on the walk rather than gathered by a second one: the edges that decide which
    /// `add_host` tasks count are the same edges the definitions walk already follows, and
    /// two traversals of one graph is how the two of them come to disagree.
    pub created_hosts: std::collections::HashSet<String>,
    /// Whether the created set is **not** enumerable — a templated `add_host` name
    /// (`name: "{{ item }}"`) or an include edge we could not resolve
    /// (`include_tasks: "{{ kind }}.yml"`) appeared somewhere in the reachable set.
    ///
    /// Separate from an empty `created_hosts`, for the reason [`crate::vars::inventory_hosts`]
    /// returns an `Option`: "creates nothing" and "creates something I cannot name" must not
    /// collapse, or a rule answering from the partial set reports a typo on a real host.
    pub hosts_unknowable: bool,
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

/// The extensions a `group_vars/`/`host_vars/` entry may carry, in Ansible's lookup order.
///
/// `DataLoader.find_vars_files` tries `[''] + YAML_FILENAME_EXTENSIONS` and **breaks on the
/// first hit**, so `all` beats `all.yml` beats `all.yaml` beats `all.json`. Measured on
/// 2.21.2 by writing all four and deleting them one at a time: four different values, so the
/// order is the file's, not the reader's.
///
/// `ini` and `toml` are deliberately absent, though an *inventory* may be either. These files
/// are loaded by the `host_group_vars` vars plugin, which only ever calls `from_yaml` — there
/// is no INI or TOML path on that side at all. Measured: a `group_vars/web.ini` holding valid
/// YAML is not read, so "it failed to parse" is ruled out and it is simply never looked at.
const VARS_EXTS: &[&str] = &["", "yml", "yaml", "json"];

/// Guards against a symlink cycle turning the recursive vars-directory scan into a hang.
const MAX_VARS_DEPTH: usize = 32;

/// Where `path`'s extension sits in [`VARS_EXTS`], or `None` if Ansible would not load it.
fn vars_ext_rank(path: &Path) -> Option<usize> {
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    VARS_EXTS.iter().position(|x| *x == ext)
}

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
    /// The Ansible install every [`FileContext`] this cache builds is given (T-201 box 4).
    /// `None` until startup has detected one — a missing answer, never a wrong one.
    install: Option<Arc<crate::install::AnsibleInstall>>,
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
            install: None,
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

    /// The Ansible install to hand every context this cache builds. Set it before any context
    /// is computed, since contexts are memoized per directory.
    pub fn with_install(mut self, install: Option<Arc<crate::install::AnsibleInstall>>) -> Self {
        self.install = install;
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
            Arc::new(
                FileContext::discover_with(file, self, |root| self.config(root))
                    .with_install(self.install.clone()),
            )
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

    /// One `group_vars/`/`host_vars/` directory's live files, memoized, and empty when `dir`
    /// isn't a directory.
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
        // Ansible resolves these BY ENTITY NAME — `find_vars_files(group_vars_dir, "webservers")`
        // — trying each extension in turn and stopping at the first that exists. We do not
        // know the group and host names (that needs the membership graph, T-062 box 8), so we
        // enumerate the directory instead and apply the same rule per *stem*: one winner per
        // name, chosen by the same order. Enumerating is a deliberate over-approximation of
        // which entities exist; it must not become an over-approximation of which files load.
        let mut best: std::collections::BTreeMap<std::ffi::OsString, (usize, PathBuf, crate::fs::Kind)> =
            std::collections::BTreeMap::new();
        for (p, k) in Fs::read_dir(self, dir) {
            let Some(name) = p.file_name().and_then(|s| s.to_str()) else { continue };
            if name.starts_with('.') || name.ends_with('~') {
                continue;
            }
            let Some(rank) = vars_ext_rank(&p) else { continue };
            let stem = p.file_stem().unwrap_or_default().to_os_string();
            if best.get(&stem).is_none_or(|(seen, _, _)| rank < *seen) {
                best.insert(stem, (rank, p, k));
            }
        }
        let mut out = Vec::new();
        for (_, (_, p, kind)) in best {
            if kind == crate::fs::Kind::Dir {
                self.collect_vars_dir(&p, 0, &mut out);
            } else {
                out.push(p);
            }
        }
        let files = Arc::new(out);
        self.listings.insert(dir.to_path_buf(), files.clone());
        files
    }

    /// One entity directory's files, in the order `_get_dir_vars_files` yields them.
    ///
    /// Subdirectories are descended, but only extension-less ones — `group_vars/web/sub.yml/`
    /// is neither read nor recursed. Hidden and `~` backup entries are skipped here too,
    /// which the old reader did not do: it read every file in the directory whatever its
    /// name, so `.hidden.yml`, `c.yml~` and `d.txt` all became definitions no run has.
    fn collect_vars_dir(&self, dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > MAX_VARS_DEPTH {
            return;
        }
        let mut entries: Vec<(PathBuf, crate::fs::Kind)> = Fs::read_dir(self, dir);
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (p, kind) in entries {
            let Some(name) = p.file_name().and_then(|s| s.to_str()) else { continue };
            if name.starts_with('.') || name.ends_with('~') {
                continue;
            }
            let ext = p.extension().and_then(|s| s.to_str());
            match kind {
                crate::fs::Kind::Dir if ext.is_none() => self.collect_vars_dir(&p, depth + 1, out),
                crate::fs::Kind::Dir => {}
                _ if vars_ext_rank(&p).is_some() => out.push(p),
                _ => {}
            }
        }
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

#[cfg(test)]
mod tests {

    /// T-201 box 4: the install a cache is given reaches every context it builds.
    ///
    /// One line (`context()`'s `.with_install`), and nothing else in the workspace crosses it:
    /// replacing it with `None` left the whole suite green. Without it the editor's module
    /// resolution silently loses the Ansible install — "module not found" on a module that is
    /// installed, which is the class of wrong answer this repo exists to avoid.
    #[test]
    fn the_install_a_cache_carries_reaches_the_contexts_it_builds() {
        let install = std::sync::Arc::new(crate::install::AnsibleInstall {
            package_dir: Some(PathBuf::from("/fake/ansible")),
            ..Default::default()
        });
        let file = Path::new("/p/play.yml");

        // Control first: a cache with no install hands out contexts with none, so the
        // assertion below cannot pass by accident on a default-populated field.
        let bare = ScanCache::new(crate::testing::MemFs::new(&[("/p/play.yml", "")]));
        assert!(bare.context(file).install.is_none(), "no install in, none out");

        let with = ScanCache::new(crate::testing::MemFs::new(&[("/p/play.yml", "")]))
            .with_install(Some(install.clone()));
        assert_eq!(
            with.context(file).install.as_ref().and_then(|i| i.package_dir.clone()),
            install.package_dir.clone(),
            "the context must carry the cache's install"
        );
    }

    use super::*;
    use crate::fs::Counting;

    fn tree(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("ansible.cfg"), "[defaults]\n").unwrap();
        std::fs::write(d.join("a.yml"), "- hosts: all\n  tasks: []\n").unwrap();
        std::fs::write(d.join("sub/b.yml"), "- debug:\n    msg: hi\n").unwrap();
        d
    }

    /// The cache exists to make the *second* ask free, so that is what this asserts — not
    /// that the answers match, which a cache that re-read everything would also satisfy.
    ///
    /// Counting the backend is the only way to see it: a wrapper outside the cache counts
    /// asks, this one sits underneath and counts what actually reached the disk.
    #[test]
    fn a_second_ask_is_answered_without_touching_the_disk() {
        let d = tree("ansible-lsp-cache-memo");
        let cache = ScanCache::new(Counting::new(StdFs));
        // `backend()` hands back the layer a miss falls through to, which is where the
        // syscall tallies live — the only way to see that a hit avoided the disk.
        assert!(cache.backend().is_file(&d.join("a.yml")), "the backend answers for itself");

        let first = cache.source(&d.join("a.yml")).expect("the file reads");
        let again = cache.source(&d.join("a.yml")).expect("and reads again");
        assert_eq!(first.text, again.text);
        assert_eq!(cache.stats().reads, 1, "the second ask did not re-read");

        // A different file is a different key.
        cache.source(&d.join("sub/b.yml")).unwrap();
        assert_eq!(cache.stats().reads, 2);

        // A remembered *failure* is a hit too — asking twice for something absent must not
        // probe twice, which is the half a cache that stores only successes gets wrong.
        assert!(cache.source(&d.join("nope.yml")).is_none());
        let after = cache.stats().reads;
        assert!(cache.source(&d.join("nope.yml")).is_none());
        assert_eq!(cache.stats().reads, after, "a miss is remembered as a miss");
    }

    /// `prime` exists so the scan does not read a file twice — once as the subject of its
    /// own analysis, once when another file's walk reaches it.
    #[test]
    fn a_primed_file_is_never_read_from_disk() {
        let d = tree("ansible-lsp-cache-prime");
        let cache = ScanCache::new(Counting::new(StdFs));
        let path = d.join("a.yml");
        let text = "- hosts: primed\n  tasks: []\n";
        let nodes = Document::new(text.to_string()).parse().unwrap();

        cache.prime(&path, text, &nodes);
        let got = cache.source(&path).expect("the primed entry answers");
        assert_eq!(&*got.text, text, "the primed text, not what is on disk");
        assert_eq!(cache.stats().reads, 0, "priming means no read at all");

        // Priming something that does not exist cannot key itself, so it is a no-op rather
        // than an entry under a path nothing will ask for.
        cache.prime(&d.join("ghost.yml"), "x", &[]);
        assert!(cache.source(&d.join("ghost.yml")).is_none());
    }

    /// Directory-shaped memos, and the counters that report them.
    #[test]
    fn contexts_trees_and_listings_are_each_memoized_once() {
        let d = tree("ansible-lsp-cache-dirs");
        let cache = ScanCache::default();

        // A context is keyed by *directory*, so two files in one directory share it.
        let c1 = cache.context(&d.join("a.yml"));
        let c2 = cache.context(&d.join("other.yml"));
        assert!(Arc::ptr_eq(&c1, &c2), "same directory, same context");
        assert_eq!(cache.stats().contexts, 1);
        cache.context(&d.join("sub/b.yml"));
        assert_eq!(cache.stats().contexts, 2, "a different directory is a different key");

        // `tree` and `listing` both memoize; asking twice returns the same Arc.
        let t1 = cache.tree(&d);
        let t2 = cache.tree(&d);
        assert!(Arc::ptr_eq(&t1, &t2));
        let l1 = cache.listing(&d);
        assert!(Arc::ptr_eq(&l1, &cache.listing(&d)));
        // Both descend — a vars directory is read however deep, the same way ansible reads
        // everything under `group_vars/all/`. What separates them is which files they accept.
        let has = |v: &[PathBuf], n: &str| v.iter().any(|p| p.file_name().is_some_and(|f| f == n));
        assert!(has(&t1, "b.yml"), "tree descends: {t1:?}");
        assert!(has(&l1, "b.yml"), "listing descends too: {l1:?}");

        // `listing` applies the vars-file extension rules; `tree` is every YAML file. An
        // extension ansible would not load for vars is the case that tells them apart.
        std::fs::write(d.join("notes.txt"), "x: 1\n").unwrap();
        std::fs::write(d.join("c.json"), "{\"y\": 2}\n").unwrap();
        let fresh = ScanCache::default();
        let listed = fresh.listing(&d);
        assert!(has(&listed, "c.json"), "json is a vars extension: {listed:?}");
        assert!(!has(&listed, "notes.txt"), "txt is not: {listed:?}");
        assert!(!has(&fresh.tree(&d), "notes.txt"), "tree is yaml files only");

        // Neither invents anything for a path that is not a directory.
        assert!(fresh.listing(&d.join("nope")).is_empty(), "no such directory");
        assert!(fresh.listing(&d.join("a.yml")).is_empty(), "a file is not a directory");
    }

    /// Contributions are stored and served by path, and the two counters callers tick by
    /// hand report what the walk could not memoize.
    #[test]
    fn contributions_round_trip_and_the_hand_counters_add_up() {
        let d = tree("ansible-lsp-cache-contrib");
        let cache = ScanCache::default();
        let key = d.join("a.yml");

        assert!(cache.contribution(&key).is_none(), "nothing stored yet");
        cache.store(key.clone(), Arc::new(Contribution::default()));
        assert!(cache.contribution(&key).is_some(), "and now it answers");

        cache.count_uncached();
        cache.count_uncached();
        cache.count_defs(5);
        cache.count_defs(3);
        let s = cache.stats();
        assert_eq!(s.uncached, 2);
        assert_eq!(s.defs, 8, "defs is a running sum, not a last-value");

        // `stats()` is a snapshot: taking it twice with no work between gives the same
        // numbers, so a report cannot drift while it is being rendered.
        assert_eq!(cache.stats().defs, s.defs);
    }

    /// The builders are the seam tests use to keep a fixture from inheriting the shell's
    /// environment, so each must actually take effect.
    #[test]
    fn the_builders_replace_the_environment_and_the_inventory() {
        let d = tree("ansible-lsp-cache-builders");
        let inv = vec![d.join("hosts.ini")];
        let cache = ScanCache::default().with_inventory(inv.clone()).with_env(EnvMap::empty());
        let ctx = cache.context(&d.join("a.yml"));
        assert_eq!(ctx.config.inventory.as_ref(), Some(&inv), "the override reaches the config");

        // Without it, the same tree resolves no inventory of its own (the fixture's
        // ansible.cfg names none).
        let plain = ScanCache::default().with_env(EnvMap::empty());
        assert!(plain.context(&d.join("a.yml")).config.inventory.is_none());
    }

    /// `canonical` resolves the parent once and settles the tail with one `lstat`, so the
    /// tail cases are where it can go wrong: a symlink must be followed, and two spellings
    /// of one file must land on one identity — that is what cycle detection is built on.
    #[cfg(unix)]
    #[test]
    fn canonical_settles_the_tail_and_gives_one_identity_per_file() {
        let d = tree("ansible-lsp-cache-canon");
        let cache = ScanCache::default();
        let direct = d.join("a.yml");
        let roundabout = d.join("sub").join("..").join("a.yml");

        assert_eq!(Fs::canonical(&cache, &direct), Fs::canonical(&cache, &roundabout));
        assert!(Fs::canonical(&cache, &d.join("nope")).is_none());

        // A symlinked tail is resolved rather than reported as itself — the `Kind::Other`
        // branch, which is the whole reason the fast path checks `symlink_kind` at all.
        let link = d.join("link.yml");
        std::os::unix::fs::symlink(&direct, &link).unwrap();
        assert_eq!(
            Fs::canonical(&cache, &link),
            Fs::canonical(&cache, &direct),
            "a link and its target are one identity"
        );

        // A `..` tail has no name to append, so it falls through to the real thing.
        assert_eq!(
            Fs::canonical(&cache, &d.join("sub").join("..")),
            Fs::canonical(&cache, &d)
        );
    }
}
