//! Thin LSP shim over `ansible-core`. All logic lives in the core crate.

mod md;

use md::{Md, Prose};

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ansible_core::cache::ScanCache;
use ansible_core::config::DuplicateDictKey;
use ansible_core::expressions;
use ansible_core::fs::{Counting, Fs, StdFs};
use ansible_core::include_target;
use ansible_core::injected;
use ansible_core::jinja;
use ansible_core::install::{AnsibleInstall, Version};
use ansible_core::attributes;
use ansible_core::complex_key;
use ansible_core::condition;
use ansible_core::mutation;
use ansible_core::vars_files;
use ansible_core::parse::{Document, Loader, Node, Span};
use ansible_core::placement;
use ansible_core::references::{self, Reference, ReferenceKind};
use ansible_core::resolve::{self, rule_id_for, Resolution, SkipReason, Status};
use ansible_core::static_fields;
use ansible_core::vars;
use ansible_core::workspace::{yaml_files, FileContext};

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use serde::{Deserialize, Serialize};
use tower_lsp::{Client, LanguageServer, LspService, Server};

/// Payload for `ansible/references`, the custom request the client uses to paint
/// resolvable references. Separate from documentLink because a link's target hijacks
/// Cmd+click — fine for one target, wrong when a templated path has several.
#[derive(Debug, Deserialize)]
struct ReferencesParams {
    uri: Url,
}

#[derive(Debug, Serialize)]
struct ResolvedRef {
    range: Range,
    /// How many files this could reach. >1 means Cmd+click opens a picker.
    targets: usize,
    /// `"reference"` (file/role/module) or `"variable"`, so the client can colour them
    /// differently.
    kind: &'static str,
}

/// User preferences that change what we volunteer, never what we report. Diagnostics are
/// deliberately not configurable here — a warning you asked for is not fluff, and turning
/// rules off belongs in a committed project file (T-025), not a per-machine setting.
#[derive(Clone, Copy)]
struct Settings {
    hints: bool,
    /// Show the candidates hover on a reference that *resolved* to a single target. Off by
    /// default: the winning target is already a Cmd+click away, so the list is noise unless
    /// asked for. (A retired `candidatesOnMissing` key once gated a hover on missing refs;
    /// that hover duplicated the diagnostic message VS Code already renders in the same
    /// tooltip, so both the hover and the key are gone.)
    candidates_on_resolved: bool,
    /// Files the workspace scan analyses at once; 0 picks one per core. A knob because a
    /// local disk peaks at `nproc` while a network mount, where the tasks are blocked rather
    /// than running, wants more — and the server can't tell which it's on.
    scan_concurrency: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hints: true,
            candidates_on_resolved: false,
            scan_concurrency: 0,
        }
    }
}

impl State {
    /// Remember `ansibleLsp.inventory` — the user stating the `-i` they run with, which an
    /// editor can never observe (T-062). Stored raw and resolved against the workspace root
    /// only when read, because settings arrive during `initialize` and the roots may not be
    /// known yet at that moment.
    fn set_inventory(&self, v: &serde_json::Value) {
        let paths: Vec<PathBuf> = v
            .get("inventory")
            .and_then(|i| i.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|p| p.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();
        if let Ok(mut slot) = self.inventory.lock() {
            *slot = paths;
        }
        // A changed inventory changes what every file can see, so nothing computed under
        // the old one may survive. Wholesale, not per-file: the setting is not a file edit
        // and has no dependency edge to walk back from.
        if let Ok(mut c) = self.var_cache.lock() {
            c.epoch = c.epoch.wrapping_add(1);
            c.entries.clear();
            c.deps.clear();
            c.reverse.clear();
        }
    }
}

impl Settings {
    /// Reads `{ inlayHints: { enabled }, hover: { candidatesOnResolved } }`.
    /// The client normalises both `initializationOptions` and `didChangeConfiguration` to this
    /// one shape, so the server doesn't have to know how VS Code nests things. Anything missing
    /// keeps its default rather than silently turning a feature off.
    fn from_json(v: &serde_json::Value) -> Self {
        let d = Self::default();
        let get = |section: &str, key: &str, fallback: bool| {
            v.get(section)
                .and_then(|h| h.get(key))
                .and_then(|b| b.as_bool())
                .unwrap_or(fallback)
        };
        Self {
            hints: get("inlayHints", "enabled", d.hints),
            candidates_on_resolved: get("hover", "candidatesOnResolved", d.candidates_on_resolved),
            scan_concurrency: v
                .get("scan")
                .and_then(|s| s.get("concurrency"))
                .and_then(|n| n.as_u64())
                .map_or(d.scan_concurrency, |n| n as usize),
        }
    }

    /// The setting resolved to a usable task count — `0` becomes one per core.
    fn in_flight(&self) -> usize {
        match self.scan_concurrency {
            0 => std::thread::available_parallelism().map_or(4, |n| n.get()),
            n => n,
        }
    }
}

/// Server -> client: whether an Ansible install was found. Drives the client's status bar,
/// which (unlike a startup toast) stays visible until it's resolved.
enum AnsibleStatus {}
impl tower_lsp::lsp_types::notification::Notification for AnsibleStatus {
    type Params = serde_json::Value;
    const METHOD: &'static str = "ansible/status";
}

/// What inventory the server settled on, pushed to the client so the status bar can show
/// it (T-062). The whole point of the ticket is that "which inventory?" is ambiguous, so a
/// tool that picks one silently reproduces the problem it is solving — the answer has to be
/// on screen.
enum InventoryStatus {}
impl tower_lsp::lsp_types::notification::Notification for InventoryStatus {
    type Params = serde_json::Value;
    const METHOD: &'static str = "ansible/inventory";
}

// ---------------------------------------------------------- variable cache (T-055)
// `vars::definitions` walks the filesystem (reads and parses every included file, role
// vars, group_vars…) and is called from several per-keystroke paths. Cache each file's
// result together with the set of files it read, and keep a reverse map file -> dependents,
// so an edit invalidates exactly the entries that read the changed file — and nothing else.
/// A cache entry is identified by the file **and** the inventory it was computed under
/// (T-201). Keying by path alone was invisible while the setting lived in a process global —
/// there was only ever one inventory in flight, and changing it cleared the whole cache. Once
/// the setting travels with the request that stops being true: two requests can legitimately
/// carry different inventories, and a path-only key hands the second the first one's answer.
type VarKey = (PathBuf, u64);

/// A stable digest of the resolved `ansibleLsp.inventory` paths, for [`VarKey`]. Empty (the
/// unconfigured case) must hash to something, and does — it is a key like any other.
fn inventory_key(inv: &[PathBuf]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    inv.len().hash(&mut h);
    for p in inv {
        p.hash(&mut h);
    }
    h.finish()
}

#[derive(Default)]
struct VarCache {
    entries: HashMap<VarKey, Arc<Vec<vars::Located>>>,
    deps: HashMap<VarKey, HashSet<PathBuf>>,
    reverse: HashMap<PathBuf, HashSet<VarKey>>,
    /// Bumped by every invalidation. The scan (detached since T-075) computes entries from
    /// disk while edits arrive; an entry whose compute straddled an invalidation must not
    /// be inserted, or a result read from pre-edit content outlives the edit.
    epoch: u64,
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Cached `vars::definitions`. On a miss, compute it and record its dependency files in the
/// reverse map so later invalidation is precise.
fn cached_definitions(
    path: &Path,
    nodes: &[Node],
    open: &OpenDocs,
    inv: &[PathBuf],
    cache: &Mutex<VarCache>,
    install: Option<&Arc<AnsibleInstall>>,
) -> Arc<Vec<vars::Located>> {
    cached_definitions_in(
        path,
        nodes,
        &ScanCache::new(OverlayFs(open.clone()))
            .with_inventory(inv.to_vec())
            .with_install(install.cloned()),
        inv,
        cache,
    )
}


/// The editor's unsaved buffers, as a read-only snapshot keyed by canonical path (T-199).
///
/// Passed to every read that answers a question about a file *other* than the one the cursor
/// is in — those went straight to disk, so an open, edited file was answered from its saved
/// text while the screen showed something else. Empty means "nothing is open", which is the
/// honest state for a workspace scan and for any caller that has no editor behind it.
///
/// A value rather than a field on `State`: the readers are free functions doing rendering,
/// and threading the whole server state through them to reach one string lookup would put
/// the client handle and the scan flag in scope of a hover renderer. It is also a *snapshot*,
/// taken once per request, so one answer cannot mix a value read before an edit with a line
/// number read after it.
///
/// T-199 asked whether a jump into a dirty buffer should be exact or refused. **Exact.**
/// Refusing would have been the honest fallback only if the buffers could not reach the
/// definitions — but [`ansible_core::fs::Fs`] is already the crate's one door to the
/// filesystem, so laying the buffers over it costs one `read` override and every consumer
/// gets the same text. Silence would have been the cheaper answer to a problem we do not have.
#[derive(Default, Clone)]
struct OpenDocs(HashMap<PathBuf, String>);

impl OpenDocs {
    fn text_of(&self, p: &Path) -> Option<&str> {
        self.0.get(&canon(p)).map(String::as_str)
    }

    /// The file's text as the editor has it, falling back to disk. The one call every
    /// former `read_to_string` site becomes.
    fn read(&self, p: &Path) -> Option<String> {
        match self.text_of(p) {
            Some(t) => Some(t.to_string()),
            None => std::fs::read_to_string(p).ok(),
        }
    }

}

/// [`StdFs`] with the open buffers layered over it, so the *definitions* — which are built
/// inside ansible-core, behind the [`ansible_core::fs::Fs`] seam — see the same text the
/// rendering does. Only [`read`](ansible_core::fs::Fs::read) differs: a buffer is a file that
/// already exists, so nothing about the shape of the tree changes.
struct OverlayFs(OpenDocs);

impl Fs for OverlayFs {
    fn kind(&self, p: &Path) -> Option<ansible_core::fs::Kind> {
        StdFs.kind(p)
    }
    fn symlink_kind(&self, p: &Path) -> Option<ansible_core::fs::Kind> {
        StdFs.symlink_kind(p)
    }
    fn read(&self, p: &Path) -> Option<String> {
        self.0.read(p)
    }
    fn is_executable(&self, p: &Path) -> bool {
        StdFs.is_executable(p)
    }
    fn read_dir(&self, p: &Path) -> Vec<(PathBuf, ansible_core::fs::Kind)> {
        StdFs.read_dir(p)
    }
    fn walk(&self, root: &Path) -> Vec<(PathBuf, Vec<String>)> {
        StdFs.walk(root)
    }
    fn canonical(&self, p: &Path) -> Option<PathBuf> {
        StdFs.canonical(p)
    }
}

/// [`cached_definitions`] against a caller-owned [`ScanCache`], so the files of one workspace
/// scan share the subtrees they all reach instead of re-walking them each (T-076). The two
/// caches answer different questions: this one keys whole results by file and survives until
/// an edit invalidates it; the scan cache keys raw per-file contributions and dies with the
/// pass.
fn cached_definitions_in(
    path: &Path,
    nodes: &[Node],
    scan: &ScanCache,
    inv: &[PathBuf],
    cache: &Mutex<VarCache>,
) -> Arc<Vec<vars::Located>> {
    let key = (canon(path), inventory_key(inv));
    let epoch = match cache.lock() {
        Ok(c) => {
            if let Some(hit) = c.entries.get(&key) {
                return hit.clone();
            }
            c.epoch
        }
        Err(_) => return Arc::new(vars::definitions_with_deps_in(path, nodes, scan).0),
    };
    let (result, deps) = vars::definitions_with_deps_in(path, nodes, scan);
    let arc = Arc::new(result);
    if let Ok(mut cache) = cache.lock() {
        // An invalidation landed while we computed — this result may predate the edit.
        // Return it uncached; the next call recomputes from current content.
        if cache.epoch == epoch {
            for f in &deps {
                cache.reverse.entry(f.clone()).or_default().insert(key.clone());
            }

            cache.deps.insert(key.clone(), deps);
            cache.entries.insert(key, arc.clone());
        }
    }
    arc
}

/// Drop every cache entry that read `file` (via the reverse map), plus one keyed by `file`
/// itself — so editing a playbook OR any file it includes recomputes precisely.
/// Drop the memoized `.j2` -> render-site map when a YAML file changes.
///
/// Wholesale, and keyed on nothing: any task file may gain or lose a `template:` task, and a
/// map from templates to call sites has no way to say which templates that touches without
/// recomputing the thing being invalidated. Coarse on purpose — **editing a `.j2` never lands
/// here**, so the per-keystroke case this cache exists for stays warm.
fn invalidate_render_sites(state: &State, file: &Path) {
    // The grammar map depends on the include graph, which lives in the `.j2` files
    // themselves — so unlike the render sites it *must* drop on a template edit too.
    if let Ok(mut c) = state.template_grammars.lock() {
        c.clear();
    }
    if Backend::is_template_file(file) {
        return;
    }
    if let Ok(mut c) = state.render_sites.lock() {
        c.clear();
    }
}

fn invalidate_var_cache(slot: &Mutex<VarCache>, file: &Path) {
    let f = canon(file);
    let Ok(mut cache) = slot.lock() else {
        return;
    };
    cache.epoch = cache.epoch.wrapping_add(1);
    let mut keys: HashSet<VarKey> = cache.reverse.get(&f).cloned().unwrap_or_default();
    // Every inventory this file was computed under, not just one: the key carries an
    // inventory digest now, so "the entry for this file" is a set.
    keys.extend(cache.entries.keys().filter(|(p, _)| *p == f).cloned());
    for key in keys {
        if let Some(dep_set) = cache.deps.remove(&key) {
            for d in dep_set {
                if let Some(r) = cache.reverse.get_mut(&d) {
                    r.remove(&key);
                }
            }
        }
        cache.entries.remove(&key);
    }
}

/// Server state, split from `Backend` so the workspace scan can run detached (T-075):
/// handlers only ever get `&self`, so anything a spawned task shares has to sit behind its
/// own `Arc`. Locks are per field and held lock-copy-release only, never across an
/// `.await` — that is what lets requests run while the scan works.
struct State {
    docs: Mutex<HashMap<Url, String>>,
    /// Workspace folders, for the repo-wide scan.
    roots: Mutex<Vec<PathBuf>>,
    /// URIs we've published non-empty diagnostics for, so a later scan can clear
    /// the ones that got fixed.
    flagged: Mutex<HashSet<Url>>,
    /// Variables each imported playbook mutates while running. Computing this walks the
    /// playbook, its roles and its includes, so it must not happen per keystroke.
    /// Invalidated wholesale by the workspace scan; T-012 will do it precisely.
    mutations: Mutex<HashMap<PathBuf, std::sync::Arc<HashSet<String>>>>,
    settings: Mutex<Settings>,
    /// The inventory the user says they run with — `ansibleLsp.inventory`, standing in for
    /// `-i`, which never reaches an editor (T-062). Empty means "model a plain
    /// `ansible-playbook`": `ANSIBLE_INVENTORY`, then `ansible.cfg`, then
    /// `/etc/ansible/hosts`. Workspace state rather than a `Settings` field because
    /// `Settings` is `Copy` and passed by value into every hover.
    inventory: Mutex<Vec<PathBuf>>,
    /// What arrived in `initializationOptions`, logged once the client can receive it.
    startup_note: Mutex<String>,
    /// True while a workspace scan runs — a second trigger during one would double-publish.
    scanning: AtomicBool,
    /// `.j2` -> the tasks that render it, memoized.
    ///
    /// `render_sites` walks and reads the whole workspace, and the server asks for it on every
    /// diagnostic publish and every jump inside a template. Measured on a generated tree at the
    /// scale the tickets cite: **42 ms at 731 files, 156 ms at 3000** — per keystroke, which is
    /// not a cost an editor can pay. The breakdown says why a cache and not a faster walk: at
    /// 3000 files the directory walk is 5 ms and *reading* the files is 75 ms, so there is no
    /// version of this that is cheap to redo.
    ///
    /// Cleared wholesale whenever a YAML file changes, because any of them may add or remove a
    /// `template:` task. That is coarse and it is the right coarseness here: **editing a `.j2`
    /// never touches YAML**, so the case that hurts — typing in a template — keeps the cache
    /// warm, and the case that clears it pays once. T-012's watcher and T-020's reverse index
    /// are where a precise version would live.
    render_sites: Mutex<HashMap<PathBuf, std::sync::Arc<Vec<resolve::RenderSite>>>>,
    /// The include graph's grammars, per workspace root, on the same invalidation as
    /// [`State::render_sites`] — and for the same reason: `template_grammars` walks every YAML
    /// file and every template, so asking per request is a scan per keystroke.
    ///
    /// Keyed by root and not by template: it is computed for the whole tree in one pass
    /// because a child can only be read once its parent's grammar is known, so there is no
    /// cheaper per-file version to cache.
    template_grammars:
        Mutex<HashMap<PathBuf, std::sync::Arc<HashMap<PathBuf, resolve::TemplateGrammar>>>>,
    /// The variable-index cache (T-055), owned rather than process-global (T-201).
    ///
    /// Every reader reaches it through this `Arc<State>`, including the detached workspace
    /// scan, so it is still one cache shared by every request in a server — what changed is
    /// that a *second* server, or a second test, no longer shares it. It was the least
    /// dangerous of the three globals (keyed by path, so a stale read is a recompute, not a
    /// wrong answer) and the last to move.
    var_cache: Mutex<VarCache>,
    /// The client's `ansibleLsp.ansiblePath` — which Ansible to index, when several exist or
    /// none is on PATH. Held here rather than pushed into a slot inside `ansible-core`
    /// (T-201 box 5): `initialize` records it, `startup` hands it to
    /// [`AnsibleInstall::init`], and nothing on a request path can reach it.
    ansible_path: Mutex<Option<PathBuf>>,
    /// The detected Ansible install (T-201 box 4), owned here rather than in a process-wide
    /// `OnceLock` inside `ansible-core`. `None` until `startup` has detected one; every
    /// reader takes it from the `FileContext` it was given, so "not detected yet" is a
    /// missing answer rather than a wrong one.
    install: Mutex<Option<Arc<ansible_core::install::AnsibleInstall>>>,
    /// The workspace scan `initialized` starts.
    ///
    /// Held rather than discarded. A bare `tokio::spawn` drops the handle, and with it both
    /// the task's panic — a scan that dies takes every diagnostic with it and says nothing —
    /// and any way to know it ran. `shutdown` has somewhere to `abort()` from now, and a test
    /// has something exact to await instead of polling for an effect.
    scan_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

struct Backend {
    client: Client,
    state: Arc<State>,
}

struct Analysis {
    /// The editor's buffers at the moment this analysis was taken (T-199). Held here so the
    /// diagnostics answer from the same text the hover does — the two contradicting each
    /// other over one variable is the incident behind rule 3.
    open: OpenDocs,
    doc: Document,
    nodes: Vec<Node>,
    ctx: FileContext,
    refs: Vec<(Reference, Resolution)>,
    /// T-110 rows 5 and 23, computed during analysis rather than at publish time: they need
    /// the *target* file's parse, and the scan cache that already holds it is only live here.
    include_targets: Vec<placement::Problem>,
    /// This file is a role's `meta/main.yml` (T-147). Carried rather than recomputed because
    /// `Analysis` has no path and the answer is `ctx` + path, which only the analyze call has.
    is_role_metadata: bool,
}

/// Per-phase time accumulated across a workspace scan (T-074). Sums, not per-file — the
/// startup log reports where the scan spends its time so the perf follow-ups target the
/// real cost instead of a guess. The phases don't add up to the wall clock: file walking,
/// reads, diagnostics and publishing live in the gap.
#[derive(Default)]
struct ScanTimings {
    parse: std::time::Duration,
    context: std::time::Duration,
    var_index: std::time::Duration,
    resolve: std::time::Duration,
    files: usize,
}

impl ScanTimings {
    /// Merge one file's timings into the scan total — each file is now measured inside
    /// its own `spawn_blocking` (T-075), so the accumulator can't be threaded through.
    fn add(&mut self, o: &ScanTimings) {
        self.parse += o.parse;
        self.context += o.context;
        self.var_index += o.var_index;
        self.resolve += o.resolve;
        self.files += o.files;
    }
}

impl State {
    fn text_of(&self, uri: &Url) -> Option<String> {
        self.docs.lock().ok()?.get(uri).cloned()
    }

    /// The open buffers as a path-keyed snapshot (T-199). Derived from `docs` rather than
    /// stored beside it, so there is one list of open documents and it cannot drift; the
    /// map is a handful of entries, so rebuilding it per request is not worth caching.
    /// A `Url` with no file path (`untitled:`) contributes nothing — there is no path for
    /// another file's reference to name.
    /// The `ansibleLsp.inventory` paths, workspace-resolved — a snapshot of the setting as
    /// it stands now, for one request to carry (T-201).
    ///
    /// This used to be a free function over two process globals, and that is the bug: a
    /// request-scoped setting in a slot anyone can write. One test wrote `["x.ini"]` and any
    /// test running in that window resolved its fixture's `ansible.cfg` against a file that
    /// does not exist, so no inventory was read at all and every inventory-derived variable
    /// silently vanished. Both halves were already fields here — `inventory` and `roots` —
    /// so the globals were shadow copies kept only because the readers were free functions.
    ///
    /// Resolution is deliberately unchanged: a relative path still joins to `roots.first()`.
    /// That rule is wrong in a multi-root window (T-202, measured) and changing it here would
    /// mix a behaviour fix into a de-globalising one.
    fn inventory_setting(&self) -> Vec<PathBuf> {
        let raw = self.inventory.lock().map(|v| v.clone()).unwrap_or_default();
        if raw.is_empty() {
            return raw;
        }
        let root = self.roots.lock().ok().and_then(|r| r.first().cloned());
        raw.into_iter()
            .map(|p| match (&root, p.is_absolute()) {
                (Some(r), false) => r.join(p),
                _ => p,
            })
            .collect()
    }

    /// The detected Ansible install, if startup has got there.
    fn install(&self) -> Option<Arc<ansible_core::install::AnsibleInstall>> {
        self.install.lock().ok().and_then(|i| i.clone())
    }

    /// The `ansibleLsp.ansiblePath` setting, if the client sent one.
    fn ansible_path(&self) -> Option<PathBuf> {
        self.ansible_path.lock().ok().and_then(|p| p.clone())
    }

    fn open_docs(&self) -> OpenDocs {
        let Ok(docs) = self.docs.lock() else {
            return OpenDocs::default();
        };
        OpenDocs(
            docs.iter()
                .filter_map(|(u, t)| Some((canon(&u.to_file_path().ok()?), t.clone())))
                .collect(),
        )
    }

    /// Parse `uri` and resolve every reference in it. `None` when the file isn't open,
    /// isn't a real path, or doesn't parse.
    fn analyze(&self, uri: &Url) -> Option<Analysis> {
        let text = self.text_of(uri)?;
        Backend::analyze_text_in(text, &uri.to_file_path().ok()?, &self.open_docs(), &self.var_cache)
    }

    fn track(&self, uri: &Url, diagnostics: &[Diagnostic]) {
        if let Ok(mut f) = self.flagged.lock() {
            if diagnostics.is_empty() {
                f.remove(uri);
            } else {
                f.insert(uri.clone());
            }
        }
    }

    /// Include targets that reach no file from any task that renders this template.
    ///
    /// **Only reportable now that the call sites are visible**, and it was held back until
    /// they were. A target absent from the template's own search path may still be supplied by
    /// a caller's `ansible_search_path`, so warning from the template alone would fire on
    /// repos that work. With every `template:` task that renders this file in hand, "no
    /// candidate anywhere" is a real claim.
    ///
    /// Still conservative in three places, each of which would otherwise be a false positive:
    /// a template with **no** call site we can find is left alone entirely (it may be rendered
    /// from a file we cannot see, or by something other than `template:`); a dynamic target
    /// names nothing and is silent by construction; and `{% include ... ignore missing %}` is
    /// legal by design — ansible renders it as empty rather than failing.
    fn missing_include_diagnostics(
        text: &str,
        path: &Path,
        ctx: &FileContext,
        sites: &[resolve::RenderSite],
        d: &jinja::Delimiters,
        is_root: bool,
    ) -> Vec<Diagnostic> {
        if sites.is_empty() {
            return Vec::new();
        }
        let Ok(refs) = jinja::references_in(text, d, is_root) else { return Vec::new() };
        let doc = Document::new(text.to_string());
        refs.iter()
            .filter(|r| !r.ignore_missing)
            .filter(|r| {
                resolve::template_include_candidates(&r.template, path, ctx, sites, &StdFs)
                    .is_empty()
            })
            .map(|r| {
                let (sl, sc) = doc.byte_to_lsp(r.span.start);
                let (el, ec) = doc.byte_to_lsp(r.span.end);
                Diagnostic {
                    range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
                    severity: Some(DiagnosticSeverity::WARNING),
                    source: Some("ansible-lsp".into()),
                    code: Some(NumberOrString::String("missing-template".into())),
                    // Ansible's own wording, so the message a user gets here and the one
                    // they get from a failed run are searchable as the same thing. Measured
                    // on 2.21.2: `Error rendering template: '<name>' not found in search
                    // paths: '<dir>', ...`.
                    message: format!(
                        "`{}` not found in the search paths of any task that renders this                          template. Ansible reports it as `Error rendering template` when the                          task runs on the target, not at parse time.",
                        r.template
                    ),
                    ..Default::default()
                }
            })
            .collect()
    }

    /// A `.j2` never goes down the YAML path. It is not YAML, so `unparseable` would fire on
    /// every one of them — the diagnostic would be true about the bytes and a lie about the
    /// file, which is exactly the kind of confident wrong answer this project exists not to
    /// give. Ansible reads these with `env.from_string`, so we do too.
    fn template_diagnostics(&self, uri: &Url) -> Vec<Diagnostic> {
        let (Some(text), Ok(path)) = (self.text_of(uri), uri.to_file_path()) else {
            return Vec::new();
        };
        let ctx = FileContext::discover(&path);
        // The delimiters come from whichever task renders this file. Reading a template with
        // the wrong pair invents tags that are not there, and this function is the one that
        // turns that into a red squiggle — so it has to ask.
        let root = self.roots.lock().ok().and_then(|r| r.first().cloned());
        let sites = Backend::render_sites_cached(self, &path, root.as_deref(), &ctx);
        let (d, is_root) = Backend::template_grammar_cached(self, &path, root.as_deref(), &ctx);
        let mut out =
            Self::template_diagnostics_at(&text, &ctx.config.jinja2_extensions, &d, is_root);
        // Only when the file renders: an include that names nothing is not worth saying on a
        // template that will not parse at all.
        if out.is_empty() && ctx.config.jinja2_extensions.is_empty() {
            out.extend(Self::missing_include_diagnostics(&text, &path, &ctx, &sites, &d, is_root));
        }
        out
    }

    /// The template half of [`unparseable_diagnostic`](Self::unparseable_diagnostic): one
    /// ERROR when the file will not render. Ansible does not parse a template until the task
    /// that renders it runs on the target, so this is the one diagnostic here that the
    /// runtime cannot give in time to help — it arrives at deploy, on the managed host.
    ///
    /// `d` is the grammar the file is read with and `root` says whether its own `#jinja2:`
    /// header applies — false for one that is only ever included. **One entry, no wrappers**:
    /// the convenience versions that used to sit here were called by tests and by nothing
    /// else, so those tests were exercising a path the server had stopped taking.
    fn template_diagnostics_at(
        text: &str,
        extensions: &[String],
        d: &jinja::Delimiters,
        root: bool,
    ) -> Vec<Diagnostic> {
        let Some(e) = jinja::will_not_render_in(text, d, extensions, root) else {
            return Vec::new();
        };
        let doc = Document::new(text.to_string());
        let (sl, sc) = doc.byte_to_lsp(e.span.start);
        let (el, ec) = doc.byte_to_lsp(e.span.end.max(e.span.start + 1));
        vec![Diagnostic {
            range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("ansible-lsp".into()),
            code: Some(NumberOrString::String("template-syntax".into())),
            message: format!(
                "{} — this template will not render. Ansible does not parse a template \
                 until the task that renders it runs, so this fails on the target host.",
                e.msg
            ),
            ..Default::default()
        }]
    }

    /// One ERROR at the parse-error position when an open file isn't valid YAML. The parser
    /// matches Ansible (libyaml), so this is invalid for Ansible too — a play that loads the
    /// file will fail. Empty when the file is fine, isn't open, or is `# noqa`-suppressed.
    ///
    /// A resolved inventory source gets its own message, because "a play that loads this
    /// file will fail" is a lie for exactly that file: the inventory manager discards the
    /// yaml plugin's error and retries the file as INI, surfacing failures only when
    /// *nothing* parsed (`inventory/manager.py:335`) — measured on 2.21.2, a group whose
    /// colons were forgotten came back as a *host*, exit 0, empty stderr (T-062, dossier
    /// `upstream/ansible-inventory-silence.md`).
    fn unparseable_diagnostic(&self, uri: &Url) -> Vec<Diagnostic> {
        let Some(text) = self.text_of(uri) else {
            return Vec::new();
        };
        let inventory = uri.to_file_path().ok().is_some_and(|p| {
            Self::is_inventory_source(&p, &ScanCache::default().with_inventory(self.inventory_setting()))
        });
        Self::unparseable_diagnostic_for(text, inventory)
    }

    fn unparseable_diagnostic_for(text: String, inventory: bool) -> Vec<Diagnostic> {
        let doc = Document::new(text);
        let Some(span) = doc.parse_error() else {
            return Vec::new();
        };
        let (code, message) = if inventory {
            (
                "inventory-not-yaml",
                "Invalid YAML — and Ansible will not report it: a `.yml` inventory that \
                 fails to parse is silently retried as INI, so a run proceeds against an \
                 inventory this file does not mean. References here aren't analysed.",
            )
        } else {
            (
                "unparseable",
                "Invalid YAML — Ansible's parser rejects this too, so a play that loads \
                 this file will fail. References here aren't analysed.",
            )
        };
        if doc.is_suppressed(span.start, code) {
            return Vec::new();
        }
        let (sl, sc) = doc.byte_to_lsp(span.start);
        let (el, ec) = doc.byte_to_lsp(span.end);
        vec![Diagnostic {
            range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("ansible-lsp".into()),
            code: Some(NumberOrString::String(code.into())),
            message: message.into(),
            ..Default::default()
        }]
    }

    /// Is this open file one of the resolved inventory sources? Canonicalised on both
    /// sides — the editor's URI and the config's relative spelling reach the same file by
    /// different paths.
    fn is_inventory_source(path: &Path, cache: &ScanCache) -> bool {
        let target = canon(path);
        ansible_core::inventory::sources(&cache.context(path).config, cache)
            .iter()
            .any(|s| canon(s) == target)
    }

    /// A propagating `when:` whose variable the target itself sets — see
    /// [`Reference::propagated_condition`] for which constructs those are (T-166: not just
    /// `import_playbook`).
    ///
    /// The condition is copied onto every task the target contributes and re-evaluated per
    /// task, so a `set_fact` inside flips it mid-run: everything before runs, everything
    /// after silently skips. `set_fact` is host-scoped, so a cluster can split. This is the
    /// only `when:` rule that needs to read other files.
    fn mutated_condition_diagnostics(&self, a: &Analysis) -> Vec<Diagnostic> {
        Self::mutated_condition_diagnostics_with(a, |t| self.mutated_vars(t))
    }

    /// [`mutated_condition_diagnostics`](Self::mutated_condition_diagnostics) with the
    /// cross-file lookup supplied, so the rule is reachable without a live `Backend` — the
    /// cache is the only thing that needed `&self`, and an untestable diagnostic is how a
    /// rule ends up verified by hand instead of pinned.
    fn mutated_condition_diagnostics_with(
        a: &Analysis,
        mutated_vars: impl Fn(&Path) -> std::sync::Arc<HashSet<String>>,
    ) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for (r, res) in &a.refs {
            let Some((conds, span)) = r.propagated_condition() else { continue };
            if a.doc.is_suppressed(span.start, "when-import-var-mutated") {
                continue;
            }
            let used: Vec<String> =
                conds.iter().flat_map(|c| condition::variables(c)).collect();
            if used.is_empty() {
                continue;
            }
            for target in &res.targets {
                let mutated = mutated_vars(target);
                let mut hit: Vec<&String> =
                    used.iter().filter(|v| mutated.contains(*v)).collect();
                if hit.is_empty() {
                    continue;
                }
                hit.sort();
                hit.dedup();
                let names = hit
                    .iter()
                    .map(|s| format!("`{s}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let (sl, sc) = a.doc.byte_to_lsp(span.start);
                let (el, ec) = a.doc.byte_to_lsp(span.end);
                out.push(Diagnostic {
                    range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
                    severity: Some(DiagnosticSeverity::WARNING),
                    source: Some("ansible-lsp".into()),
                    code: Some(NumberOrString::String("when-import-var-mutated".into())),
                    message: format!(
                        "{names} is set by `{}` while it runs. This `when:` is copied onto \
                         every imported task and re-evaluated per task, so it flips \
                         partway through: tasks before the assignment run, tasks after \
                         are silently skipped. `set_fact` is per-host, so hosts can \
                         diverge. Gate with a variable the import doesn't assign, or use \
                         `meta: end_play` inside it.",
                        shorten(target, &a.ctx)
                    ),
                    ..Default::default()
                });
                break;
            }
        }
        out
    }

    fn mutated_vars(&self, target: &Path) -> std::sync::Arc<HashSet<String>> {
        if let Ok(cache) = self.mutations.lock() {
            if let Some(hit) = cache.get(target) {
                return hit.clone();
            }
        }
        let computed = std::sync::Arc::new(mutation::mutated_vars(target));
        if let Ok(mut cache) = self.mutations.lock() {
            cache.insert(target.to_path_buf(), computed.clone());
        }
        computed
    }
}

impl Backend {
    /// [`analyze_text_in`](Self::analyze_text_in) with no editor behind it — every test that
    /// analyses a file on disk, where "nothing is open" is the truth rather than a default.
    #[cfg(test)]
    fn analyze_text(text: String, path: &Path) -> Option<Analysis> {
        // Its own cache, not a shared one: "no editor behind this" is the truth for these
        // callers, and a cache per call is what keeps one test's index out of the next
        // test's answer — the whole point of T-201.
        Self::analyze_text_in(text, path, &OpenDocs::default(), &Mutex::new(VarCache::default()))
    }

    /// [`analyze_text`] against the editor's open buffers (T-199) — the variable index this
    /// builds reaches *other* files, and one of those may be open and edited.
    ///
    /// The `ScanCache` here stays disk-backed on purpose: it answers "what does the tree look
    /// like", which an unsaved edit to a file's *contents* does not change. Only the variable
    /// index, which `cached_definitions` builds behind its own overlay, reads text for values.
    fn analyze_text_in(
        text: String,
        path: &Path,
        open: &OpenDocs,
        cache: &Mutex<VarCache>,
    ) -> Option<Analysis> {
        Self::analyze_text_measured(
            text,
            path,
            &mut ScanTimings::default(),
            &ScanCache::default(),
            open,
            cache,
        )
    }

    /// The body of `analyze_text`, wrapping each phase with a timer that accumulates into
    /// `t`. The un-instrumented `analyze_text` passes a throwaway accumulator and its own
    /// one-shot cache, so the logic lives in exactly one place.
    fn analyze_text_measured(
        text: String,
        path: &Path,
        t: &mut ScanTimings,
        scan: &ScanCache,
        open: &OpenDocs,
        cache: &Mutex<VarCache>,
    ) -> Option<Analysis> {
        use std::time::Instant;
        let s = Instant::now();
        let doc = Document::new(text);
        let nodes = doc.parse()?;
        // Hand this file's parse to the scan cache: some other file's variable walk will
        // reach it, and would otherwise read and parse it a second time.
        scan.prime(path, &doc.text, &nodes);
        t.parse += s.elapsed();

        let s = Instant::now();
        let ctx = (*scan.context(path)).clone();
        t.context += s.elapsed();

        // Variable values that are statically knowable, so a `{{ var }}` in a path can be
        // navigated to its real target (T-056). Navigation only — resolve_with never warns.
        let s = Instant::now();
        // `&[]`, not the editor's setting: this path builds its own `ScanCache` and never
        // called `with_inventory`, so passing anything else would change what the scan reads.
        // It also means the scan and the hover no longer share a cache entry for one file —
        // under the old path-only key whichever ran first decided whether the setting applied.
        let defs = cached_definitions_in(path, &nodes, scan, &[], cache);
        let literals = vars::known_literals_in(&defs, path, &doc.text, scan);
        t.var_index += s.elapsed();

        let s = Instant::now();
        let extracted = references::extract(&nodes);
        // One resolver for the file: `in_playbook` is a property of this file, not of any
        // one reference in it, and every field is a reference or a word so building it
        // costs no allocation (T-135).
        let resolver = resolve::Resolver {
            fs: scan,
            literals: Some(&literals),
            in_playbook: extracted.in_playbook,
            ..Default::default()
        };
        let is_role_metadata = ctx.is_role_metadata(path);
        let mut extracted = extracted.refs;
        if is_role_metadata {
            extracted.extend(references::meta_dependencies(&nodes));
        }
        let refs: Vec<(Reference, Resolution)> = extracted
            .into_iter()
            .map(|r| {
                let res = resolver.resolve(&r, &ctx);
                (r, res)
            })
            .collect();
        t.resolve += s.elapsed();

        // Only refs that already resolved: an unresolved one is `missing-file`'s to report, and
        // saying "the file it points at is empty" about a file that isn't there would be two
        // diagnostics for one fault. `scan.source` is a cache hit whenever the variable walk
        // above has been through the same file.
        let include_targets = refs
            .iter()
            .filter(|(_, res)| res.status == Status::Resolved)
            .filter_map(|(r, res)| {
                let target = res.targets.first()?;
                let src = scan.source(target)?;
                let nodes = src.nodes.as_ref()?;
                include_target::problem(r, nodes)
            })
            .collect();

        Some(Analysis { open: open.clone(), doc, nodes, ctx, refs, include_targets, is_role_metadata })
    }

    /// Warn only on literal paths that resolved to nothing. Templated values and
    /// unsupported kinds stay silent — a warning you can't trust is worse than none.
    /// Extensions the server reads as Jinja rather than YAML. Ansible does not require any
    /// of them — `template: src=x` renders whatever `x` is — but the editor has only the
    /// filename to go on, and these three are the spellings that mean "template" in practice.
    const TEMPLATE_EXTENSIONS: &'static [&'static str] = &["j2", "jinja", "jinja2"];

    fn is_template_file(path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| Self::TEMPLATE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
    }

    /// Every file the `{% include %}` target under the cursor could reach.
    ///
    /// **All of them, not one.** The search path belongs to the task doing the rendering, so a
    /// template rendered from two roles has two answers and neither is wrong — `demo/`'s
    /// `common.j2` has three. The editor shows a picker when there is more than one, which is
    /// the T-029-shaped answer T-040 asked for: offer the candidates rather than guess.
    ///
    /// Ordered by call site first, then the location-derived path that applies whoever renders
    /// the template, so the commonest answer leads.
    fn template_definitions_at(
        state: &State,
        text: &str,
        pos: Position,
        path: &Path,
        root: Option<&Path>,
    ) -> Vec<Location> {
        let doc = Document::new(text.to_string());
        let byte = doc.lsp_to_byte(pos.line, pos.character);
        let ctx = FileContext::discover(path);
        let sites = Self::render_sites_cached(state, path, root, &ctx);
        let (d, is_root) = Self::template_grammar_cached(state, path, root, &ctx);
        let Ok(refs) = jinja::references_in(text, &d, is_root) else { return Vec::new() };
        let Some(r) = refs.into_iter().find(|r| r.span.start <= byte && byte <= r.span.end)
        else {
            return Vec::new();
        };
        resolve::template_include_candidates(&r.template, path, &ctx, &sites, &StdFs)
            .into_iter()
            .filter_map(|p| {
                Some(Location {
                    uri: Url::from_file_path(p).ok()?,
                    range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                })
            })
            .collect()
    }

    /// The tasks that render `path`, or none when there is no workspace root to search.
    fn render_sites_for(
        path: &Path,
        root: Option<&Path>,
        ctx: &FileContext,
    ) -> Vec<resolve::RenderSite> {
        let root = root.map(Path::to_path_buf).or_else(|| ctx.project_root.clone());
        match root {
            Some(r) => resolve::render_sites(path, &r, &StdFs),
            None => Vec::new(),
        }
    }

    /// [`render_sites_for`](Self::render_sites_for) through [`State::render_sites`]. Every
    /// request path goes through this one, so there is a single place the cache can be wrong.
    fn render_sites_cached(
        state: &State,
        path: &Path,
        root: Option<&Path>,
        ctx: &FileContext,
    ) -> std::sync::Arc<Vec<resolve::RenderSite>> {
        let key = canon(path);
        if let Ok(c) = state.render_sites.lock() {
            if let Some(hit) = c.get(&key) {
                return hit.clone();
            }
        }
        let computed = std::sync::Arc::new(Self::render_sites_for(path, root, ctx));
        if let Ok(mut c) = state.render_sites.lock() {
            c.insert(key, computed.clone());
        }
        computed
    }

    /// The grammar a template is read with, memoized in [`State::template_grammars`].
    ///
    /// Not the call sites' delimiters alone: a partial that no task names inherits its
    /// includer's, and its own header is inert. Measured — see
    /// [`resolve::effective_delimiters`].
    /// [`template_grammar_cached`](Self::template_grammar_cached) without the compute — the
    /// memoized answer or `None`.
    ///
    /// Exists because that function walks every YAML file and every template on a miss, which
    /// is fine for a diagnostic the user waits on and wrong for colouring: measured at ~10s on
    /// the demo, during which a reloaded window shows plain text. A caller that can answer
    /// usefully without the grammar asks with this and refreshes when the map lands.
    fn template_grammar_if_cached(
        state: &State,
        path: &Path,
        root: Option<&Path>,
        ctx: &FileContext,
    ) -> Option<(jinja::Delimiters, bool)> {
        let r = root.map(Path::to_path_buf).or_else(|| ctx.project_root.clone())?;
        let map = state.template_grammars.lock().ok()?.get(&canon(&r)).cloned()?;
        Some(match map.get(&canon(path)) {
            Some(g) => (g.delimiters.clone(), g.root),
            None => (jinja::Delimiters::default(), true),
        })
    }

    fn template_grammar_cached(
        state: &State,
        path: &Path,
        root: Option<&Path>,
        ctx: &FileContext,
    ) -> (jinja::Delimiters, bool) {
        let Some(r) = root.map(Path::to_path_buf).or_else(|| ctx.project_root.clone()) else {
            return (jinja::Delimiters::default(), true);
        };
        let key = canon(&r);
        let map = {
            let hit = state.template_grammars.lock().ok().and_then(|c| c.get(&key).cloned());
            match hit {
                Some(m) => m,
                None => {
                    let m = std::sync::Arc::new(resolve::template_grammars(&r, &StdFs));
                    if let Ok(mut c) = state.template_grammars.lock() {
                        c.insert(key, m.clone());
                    }
                    m
                }
            }
        };
        match map.get(&canon(path)) {
            Some(g) => (g.delimiters.clone(), g.root),
            None => (jinja::Delimiters::default(), true),
        }
    }

    async fn publish_diagnostics(&self, uri: &Url) {
        // Before the YAML path, not inside it: a template is a different grammar, not a
        // broken document of this one.
        if uri.to_file_path().is_ok_and(|p| Self::is_template_file(&p)) {
            let diags = self.state.template_diagnostics(uri);
            self.state.track(uri, &diags);
            self.client.publish_diagnostics(uri.clone(), diags, None).await;
            return;
        }
        let Some(a) = self.state.analyze(uri) else {
            // No analysis means the file didn't parse. Since the parser now matches Ansible's
            // (libyaml), a parse failure is a real one — a play that loads this file will
            // fail — so it's an error, not a silent gap.
            let diags = self.state.unparseable_diagnostic(uri);
            self.state.track(uri, &diags);
            self.client.publish_diagnostics(uri.clone(), diags, None).await;
            return;
        };
        let mut diagnostics = Self::diagnostics_of(&a);
        diagnostics.extend(Self::duplicate_key_diagnostics(&a));
        diagnostics.extend(self.state.mutated_condition_diagnostics(&a));
        if let Ok(path) = uri.to_file_path() {
            // One snapshot for this publish: the coverage walk and the unknown-host walk must
            // answer from the same inventory, or two diagnostics on one file disagree (rule 3).
            let inv = self.state.inventory_setting();
            diagnostics.extend(Self::variable_coverage_diagnostics(&a, &path, &a.nodes, &inv, &self.state.var_cache));
            diagnostics.extend(Self::group_priority_diagnostics(&a, &path));
            let cache = ScanCache::default()
                .with_inventory(inv.clone())
                .with_install(self.state.install());
            diagnostics.extend(Self::unknown_host_diagnostics(&a, &path, &cache));
        }
        self.state.track(uri, &diagnostics);
        self.client
            .publish_diagnostics(uri.clone(), diagnostics, None)
            .await;
    }

    /// A key written twice in one mapping. Legal YAML, so the file loads and runs — the
    /// earlier value is simply gone before any play sees it, which is why an editor is the
    /// only place this is catchable. Anchored on the *later* occurrence, matching both
    /// Ansible's own warning and the value the rest of the tool now reads (T-102).
    ///
    /// Severity is the user's, via `duplicate_dict_key`: `error` genuinely refuses to load
    /// the file, `warn` is Ansible's default, and `ignore` means they have asked for
    /// silence and get it. Suppressible per-line with `# noqa: duplicate-key`.
    ///
    /// A JSON-content file behaves identically — the key is just as dead — but says so,
    /// because Ansible itself is silent there: `json.loads` runs before the YAML
    /// constructor that owns the check, so not even `error` fires. That exemption is
    /// collateral from a helper shared with `-e` extra-vars, not a decision about
    /// playbooks, so the editor is the only place the loss is visible at all.
    fn duplicate_key_diagnostics(a: &Analysis) -> Vec<Diagnostic> {
        let severity = match a.ctx.config.duplicate_dict_key {
            DuplicateDictKey::Ignore => return Vec::new(),
            DuplicateDictKey::Error => DiagnosticSeverity::ERROR,
            DuplicateDictKey::Warn => DiagnosticSeverity::WARNING,
        };
        let unreported = match a.doc.loader() {
            Loader::Yaml => "",
            Loader::Json => " Ansible does not report this one: the contents parse as JSON, \
                             so its duplicate-key check never runs.",
        };
        a.doc
            .duplicate_keys()
            .into_iter()
            .filter(|d| !a.doc.is_suppressed(d.span.start, "duplicate-key"))
            .map(|d| {
                let (sl, sc) = a.doc.byte_to_lsp(d.span.start);
                let (el, ec) = a.doc.byte_to_lsp(d.span.end);
                let (first_line, _) = a.doc.byte_to_lsp(d.first.start);
                Diagnostic {
                    range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
                    severity: Some(severity),
                    source: Some("ansible-lsp".into()),
                    code: Some(NumberOrString::String("duplicate-key".into())),
                    message: format!(
                        "Duplicate mapping key `{}` — this value wins and the one on line {} \
                         is discarded.{unreported}",
                        d.key,
                        first_line + 1
                    ),
                    ..Default::default()
                }
            })
            .collect()
    }

    /// `hostvars['name']` for a host no inventory we parsed declares.
    ///
    /// Fatal at runtime, and ansible's own message is the reason this is worth saying: the
    /// lookup returns an Undefined whose *name* is the subscript itself
    /// (`_undef(f"hostvars[{host_name!r}]")`, `vars/hostvars.py`), so the failure reads
    /// `Error while resolving value for 'msg': hostvars['web0143']` — the expression echoed
    /// back, never "there is no such host". Measured on 2.21.2, beside the control: the same
    /// play with a host that *does* exist and a missing variable says `object of type
    /// 'HostVarsVars' has no attribute 'infiniband_ip'`, which names the thing. Only the
    /// bad-host half is cryptic, and it is the half a typo produces.
    ///
    /// An ERROR because it is provable rather than stylistic — once the inventory is parsed a
    /// name either is a host or is not, with no scope, ordering or precedence in the way.
    ///
    /// Everything about it is arranged to stay quiet unless that proof holds:
    ///
    /// - the host list is `None` — no inventory resolved, unreadable, or **dynamic** and
    ///   therefore declined. [`vars::inventory_hosts`] owns that distinction.
    /// - any `add_host` in the file. It invents hosts at runtime that appear in no inventory,
    ///   and the check is deliberately the blunt textual one: an escape that over-matches
    ///   only ever costs a missed report, while one that under-matches costs a false error.
    /// - the implicit localhost, under all three of its spellings — `localhost`,
    ///   `127.0.0.1`, `::1` ([`condition::IMPLICIT_HOSTS`]). Measured: each resolves with no
    ///   inventory entry at all, because membership auto-creates the host, while
    ///   `hostvars | list` omits it — so the host list can never contain them.
    /// - an expression that swallows the undefined. `hostvars['nope'].x` is fatal;
    ///   `hostvars['nope'].x | default('z')` prints `z`, measured. The message here says the
    ///   read *fails*, and that has to remain true of every case it fires on.
    /// - a templated key. `hostvars[some_var]` names no host we can know, and the scan only
    ///   matches quoted literals, so this falls out rather than being special-cased.
    ///
    /// Suppressible with `# noqa: unknown-host`.
    fn unknown_host_diagnostics(a: &Analysis, path: &Path, cache: &ScanCache) -> Vec<Diagnostic> {
        // The candidate reads first, and bail when there are none. Both host sets below walk
        // the include graph, and a file with no `hostvars['literal']` in it cannot produce a
        // diagnostic however they come out — so computing them first made this rule cost
        // **4.4s** across the 759-file corpus against 140ms for the whole of the rest of the
        // diagnostics pass (T-179's cost box, T-131). Nearly every file takes the early exit.
        let uses = condition::hostvars_host_uses(&a.doc.text, &a.nodes);
        if uses.is_empty() {
            return Vec::new();
        }
        // Both sets or nothing. Either being unknowable means a host may exist under a name
        // we cannot produce, and the rule's claim is absence — so it has to stop answering
        // rather than answer from the half it has.
        let (Some(inventory), Some(created)) = (
            vars::inventory_hosts(path, cache),
            vars::created_hosts_in(path, &a.nodes, cache),
        ) else {
            return Vec::new();
        };
        let hosts: std::collections::HashSet<&String> = inventory.iter().chain(&created).collect();
        uses.into_iter()
            .filter(|(name, _, _)| {
                !condition::IMPLICIT_HOSTS.contains(&name.as_str()) && !hosts.contains(name)
            })
            .filter(|(_, s, _)| !a.doc.is_suppressed(*s, "unknown-host"))
            .map(|(name, s, e)| {
                let (sl, sc) = a.doc.byte_to_lsp(s);
                let (el, ec) = a.doc.byte_to_lsp(e);
                Diagnostic {
                    range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
                    severity: Some(DiagnosticSeverity::ERROR),
                    source: Some("ansible-lsp".into()),
                    code: Some(NumberOrString::String("unknown-host".into())),
                    message: format!(
                        "No host `{name}` in the inventory, so this read fails at runtime. \
                         Ansible reports it as `hostvars['{name}']` with no further \
                         explanation, which reads like a missing variable rather than a \
                         missing host."
                    ),
                    ..Default::default()
                }
            })
            .collect()
    }

    /// `ansible_group_priority` in a `group_vars/`/`host_vars/` file, which is a no-op there.
    ///
    /// A WARNING rather than an ERROR: nothing fails, the run just merges in an order the
    /// author did not ask for. Suppressible with `# noqa: group-priority-ignored`.
    ///
    /// This is the rare rule with no visible symptom to point at — the key is still there in
    /// `hostvars` afterwards, which is precisely what makes it read as accepted. The
    /// measurement, including the controls where the key *does* work, is on
    /// [`vars::ignored_group_priority`].
    fn group_priority_diagnostics(a: &Analysis, path: &Path) -> Vec<Diagnostic> {
        vars::ignored_group_priority(path, &a.nodes)
            .into_iter()
            .filter(|s| !a.doc.is_suppressed(s.start, "group-priority-ignored"))
            .map(|s| {
                let (sl, sc) = a.doc.byte_to_lsp(s.start);
                let (el, ec) = a.doc.byte_to_lsp(s.end);
                Diagnostic {
                    range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
                    severity: Some(DiagnosticSeverity::WARNING),
                    source: Some("ansible-lsp".into()),
                    code: Some(NumberOrString::String("group-priority-ignored".into())),
                    message: "`ansible_group_priority` does nothing in a `group_vars`/\
                              `host_vars` file — it is consumed while an inventory *source* is \
                              parsed, and these files are merged afterwards by a vars plugin \
                              that bypasses it. It survives here as an ordinary variable, so \
                              it looks accepted. To change merge order, set it in the \
                              inventory itself: `[<group>:vars]`, or the group's `vars:` in a \
                              YAML inventory."
                        .into(),
                    ..Default::default()
                }
            })
            .collect()
    }

    fn diagnostics_of(a: &Analysis) -> Vec<Diagnostic> {
        Self::diagnostics_with(a, a.ctx.install.as_ref().and_then(|i| i.version))
    }

    /// `core` is the detected ansible-core version, taken as an argument rather than read
    /// from the global so the version-sensitive tiers are testable without an install.
    fn diagnostics_with(a: &Analysis, core: Option<Version>) -> Vec<Diagnostic> {
        let range_of = |s: ansible_core::parse::Span| {
            let (sl, sc) = a.doc.byte_to_lsp(s.start);
            let (el, ec) = a.doc.byte_to_lsp(s.end);
            Range::new(Position::new(sl, sc), Position::new(el, ec))
        };
        let missing = a
            .refs
            .iter()
            .filter(|(_, res)| res.status == Status::Missing)
            // An `import_playbook:` inside a task list is never resolved by ansible — it is read
            // as a module name and dies on its parameters — so whether the file exists is not a
            // fact about this mistake. T-110 row `ip` says the one true thing about the line;
            // `missing-file` would be a second diagnostic describing a lookup that never runs.
            .filter(|(r, _)| {
                r.kind != ReferenceKind::ImportPlaybook || r.playbook_entry
            })
            // `template: src:` navigates but does not warn. The reference exists so T-040 can
            // link a `.j2` to the tasks that render it; the missing-file verdict on `src:` is
            // T-015's, and it is held behind that ticket's corpus gate — 385 `src:` values in
            // one real tree, and a wave of new warnings is exactly what that gate is for.
            .filter(|(r, _)| r.kind != ReferenceKind::TemplateSrc)
            .filter(|(r, res)| !a.doc.is_suppressed(r.span.start, rule_id_for(r, res)))
            .map(|(r, res)| Diagnostic {
                range: range_of(r.span),
                // A miss is a warning because the play still runs; a directory stops it
                // before its first task, which is the unparseable-file tier.
                severity: Some(match res.directory {
                    Some(_) => DiagnosticSeverity::ERROR,
                    None => DiagnosticSeverity::WARNING,
                }),
                source: Some("ansible-lsp".into()),
                code: Some(NumberOrString::String(rule_id_for(r, res).into())),
                message: message_for(r, res, &a.ctx),
                ..Default::default()
            });

        // T-184: a role re-loading its own `vars/main.yml`. Keyed on what the reference
        // resolved to, so every spelling that reaches that file is covered and no spelling has
        // to be enumerated here.
        let redundant_role_vars: Vec<Diagnostic> = a
            .refs
            .iter()
            .filter(|(r, _)| r.kind == ReferenceKind::IncludeVars)
            .filter_map(|(r, res)| {
                ansible_core::include_vars::redundant_self_reload(
                    res.targets.first().map(|p| p.as_path()),
                    a.ctx.role_dir.as_deref(),
                    r.span,
                )
            })
            .filter(|p| !a.doc.is_suppressed(p.span.start, p.rule))
            .map(|p| Diagnostic {
                range: range_of(p.span),
                severity: Some(match p.tier {
                    placement::Tier::Error => DiagnosticSeverity::ERROR,
                    placement::Tier::Warning => DiagnosticSeverity::WARNING,
                    placement::Tier::Hint => DiagnosticSeverity::HINT,
                }),
                source: Some("ansible-lsp".into()),
                code: Some(NumberOrString::String(p.rule.into())),
                message: p.message.clone(),
                ..Default::default()
            })
            .collect();

        // T-110 rows 5 and 23: the file an `import_tasks:`/`include_tasks:` points at is empty,
        // or is not a list of tasks. Anchored on the reference here, since the file at fault
        // may not be open.
        let bad_targets: Vec<Diagnostic> = a
            .include_targets
            .iter()
            .filter(|p| !a.doc.is_suppressed(p.span.start, p.rule))
            .map(|p| Diagnostic {
                range: range_of(p.span),
                severity: Some(match p.tier {
                    placement::Tier::Error => DiagnosticSeverity::ERROR,
                    placement::Tier::Warning => DiagnosticSeverity::WARNING,
                    placement::Tier::Hint => DiagnosticSeverity::HINT,
                }),
                source: Some("ansible-lsp".into()),
                code: Some(NumberOrString::String(p.rule.into())),
                message: p.message.clone(),
                ..Default::default()
            })
            .collect();

        // Expressions that cannot work whatever the variables hold, anchored on the value
        // they were written in. Read from the tree rather than from the references a task
        // produced: all five bare-expression keywords count, and a task with no reference —
        // or a `when:` on a block — is diagnosed like any other (T-141).
        //
        // 2.19 made conditionals strict, so the same fault is an error on a new core and a
        // warning-of-a-future-break on an old one. The version picks the severity, never
        // whether we speak; undetected falls to the warning, which is right either way (T-117).
        let broken: Vec<Diagnostic> = expressions::sites(&a.nodes)
            .into_iter()
            .flat_map(|s| {
                s.clauses
                    .iter()
                    .flat_map(|c| condition::problems(c, s.binds_item))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(move |p| (p, s.keyword.clone(), s.value_span))
            })
            .filter(|(p, _, span)| !a.doc.is_suppressed(span.start, p.rule_id()))
            .map(|(p, keyword, span)| Diagnostic {
                range: range_of(span),
                severity: Some(match p.tier(core) {
                    condition::Tier::Error => DiagnosticSeverity::ERROR,
                    condition::Tier::Warning => DiagnosticSeverity::WARNING,
                    condition::Tier::Hint => DiagnosticSeverity::HINT,
                }),
                source: Some("ansible-lsp".into()),
                code: Some(NumberOrString::String(p.rule_id().into())),
                message: p.message(core, &keyword),
                ..Default::default()
            })
            .collect();

        // Keys ansible-core would refuse to load — `'x' is not a valid attribute for a
        // Play/Task/...`, classified per context in `ast::build`. Task-level severity
        // follows the project's `invalid_task_attribute_failed`; play/block/loop_control
        // unknowns are errors regardless (T-107).
        // A role's `meta/main.yml` is a top-level mapping, so `ast::build` calls it
        // `Ast::Other` and the walk above never reaches it. Route it by path instead —
        // its keys are `RoleMetadata`'s, and unknown ones are fatal at load (T-147).
        let invalid: Vec<Diagnostic> = if a.is_role_metadata {
            attributes::role_metadata_problems(&a.nodes)
        } else {
            attributes::problems(
                &ansible_core::ast::build(&a.nodes),
                a.ctx.config.invalid_task_attribute_failed,
            )
        }
        .into_iter()
        .filter(|p| !a.doc.is_suppressed(p.span.start, p.rule))
        .map(|p| Diagnostic {
            range: range_of(p.span),
            severity: Some(match p.tier {
                attributes::Tier::Error => DiagnosticSeverity::ERROR,
                attributes::Tier::Warning => DiagnosticSeverity::WARNING,
            }),
            source: Some("ansible-lsp".into()),
            code: Some(NumberOrString::String(p.rule.into())),
            message: p.message,
            ..Default::default()
        })
        .collect();

        // Shapes ansible-core refuses to load, where every key involved is spelled correctly
        // and legal where it sits — what is wrong is the structure around it, which the
        // keyword sets cannot see (T-110).
        let misplaced: Vec<Diagnostic> = placement::problems(&a.nodes, &a.doc.text)
            .into_iter()
            .filter(|p| !a.doc.is_suppressed(p.span.start, p.rule))
            .map(|p| Diagnostic {
                range: range_of(p.span),
                severity: Some(match p.tier {
                    placement::Tier::Error => DiagnosticSeverity::ERROR,
                    placement::Tier::Warning => DiagnosticSeverity::WARNING,
                    placement::Tier::Hint => DiagnosticSeverity::HINT,
                }),
                source: Some("ansible-lsp".into()),
                code: Some(NumberOrString::String(p.rule.into())),
                message: p.message,
                ..Default::default()
            })
            .collect();

        // A template written in a field ansible-core never templates — `register`,
        // `listen`, `collections`, `vars:`/`module_defaults:` keys. The braces are used
        // literally, fatally or silently per field (T-103).
        let literal: Vec<Diagnostic> = static_fields::problems(&a.nodes)
            .into_iter()
            .filter(|p| !a.doc.is_suppressed(p.span.start, p.rule))
            .map(|p| Diagnostic {
                range: range_of(p.span),
                severity: Some(match p.tier {
                    static_fields::Tier::Error => DiagnosticSeverity::ERROR,
                    static_fields::Tier::Warning => DiagnosticSeverity::WARNING,
                }),
                source: Some("ansible-lsp".into()),
                code: Some(NumberOrString::String(p.rule.into())),
                message: p.message,
                ..Default::default()
            })
            .collect();

        // A non-scalar mapping key: valid YAML our libyaml parses, fatal to Ansible's
        // loader in every spelling and every document kind (T-168). Always an error.
        let unloadable: Vec<Diagnostic> = complex_key::problems(&a.nodes, &a.doc.text)
            .into_iter()
            .filter(|p| !a.doc.is_suppressed(p.span.start, p.rule))
            .map(|p| Diagnostic {
                range: range_of(p.span),
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("ansible-lsp".into()),
                code: Some(NumberOrString::String(p.rule.into())),
                message: p.message,
                ..Default::default()
            })
            .collect();

        // T-087: a `vars_files:` item that can never name a file. Read from the AST, which
        // already decided which items are entries, so the diagnostic and the navigation
        // cannot disagree about the same line. Always an error — the play never starts.
        let bad_vars_files: Vec<Diagnostic> = vars_files::problems(
            &ansible_core::ast::build(&a.nodes),
            &a.doc.text,
        )
        .into_iter()
        .filter(|p| !a.doc.is_suppressed(p.span.start, p.rule))
        .map(|p| Diagnostic {
            range: range_of(p.span),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("ansible-lsp".into()),
            code: Some(NumberOrString::String(p.rule.into())),
            message: p.message,
            ..Default::default()
        })
        .collect();

        missing
            .chain(broken)
            .chain(invalid)
            .chain(misplaced)
            .chain(literal)
            .chain(unloadable)
            .chain(bad_vars_files)
            .chain(bad_targets)
            .chain(redundant_role_vars)
            .collect()
    }

    /// Condition-aware definedness: a variable *used* under a `when:` that its *definitions*
    /// don't all cover. If the use can run in a case where no in-effect definition applies —
    /// e.g. used for `web01 or web02` but only registered on `web01` — warn, naming the
    /// uncovered case. Only fires when there IS a definition (a coverage gap, not "never
    /// defined") and only within the use's own condition vocabulary, so it can't false-warn
    /// on conditions it can't relate. Suppressible with `# noqa: var-uncovered-when`.
    fn variable_coverage_diagnostics(
        a: &Analysis,
        path: &Path,
        nodes: &[Node],
        inv: &[PathBuf],
        cache: &Mutex<VarCache>,
    ) -> Vec<Diagnostic> {
        let defs = cached_definitions(path, nodes, &a.open, inv, cache, a.ctx.install.as_ref());
        let mut out = Vec::new();
        for u in vars::uses(nodes) {
            if u.guard.is_empty() {
                continue;
            }
            // Definitions of this name that are in effect at the use.
            let def_guards: Vec<Vec<String>> = defs
                .iter()
                .filter(|d| d.name == u.name && d.in_effect_for(&u, path))
                .map(|d| d.condition.clone().into_iter().collect())
                .collect();
            if def_guards.is_empty() {
                continue;
            }
            let Some(gap) = ansible_core::guard::coverage_gap(&u.guard, &def_guards) else {
                continue;
            };
            if a.doc.is_suppressed(u.span.start, "var-uncovered-when") {
                continue;
            }
            let (sl, sc) = a.doc.byte_to_lsp(u.span.start);
            let (el, ec) = a.doc.byte_to_lsp(u.span.end);
            out.push(Diagnostic {
                range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
                severity: Some(DiagnosticSeverity::WARNING),
                source: Some("ansible-lsp".into()),
                code: Some(NumberOrString::String("var-uncovered-when".into())),
                message: format!(
                    "`{}` may be undefined when `{gap}` — used here under a broader condition \
                     than any definition covers.",
                    u.name
                ),
                ..Default::default()
            });
        }

        // T-051 base case: no reachable definition at all. The message concedes the
        // sources we cannot see. Suppressible with `# noqa: var-undefined`.
        for u in vars::undefined_uses(path, nodes, &a.doc.text) {
            if a.doc.is_suppressed(u.span.start, "var-undefined") {
                continue;
            }
            let (sl, sc) = a.doc.byte_to_lsp(u.span.start);
            let (el, ec) = a.doc.byte_to_lsp(u.span.end);
            out.push(Diagnostic {
                range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
                severity: Some(DiagnosticSeverity::WARNING),
                source: Some("ansible-lsp".into()),
                code: Some(NumberOrString::String("var-undefined".into())),
                message: if let Some(scope) = u.scope_gap {
                    scope_gap_message(&u, scope)
                } else if let Some(gone) = u.removed_in {
                    format!(
                        "`{}` was removed in ansible-core {gone}, and the detected install is {} — \
                         it is undefined here.",
                        u.name,
                        a.ctx.install.as_ref().and_then(|i| i.version).map_or("newer".to_string(), |v| v.to_string())
                    )
                } else if u.no_facts_here {
                    format!(
                        "`{}` is not a name ansible sets, and this play gathers no facts — \
                         it may still come from inventory, a fact cache from an earlier \
                         run, or extra-vars (-e).",
                        u.name
                    )
                } else if u.defined_out_of_scope {
                    // It IS defined in this file — pointing at "never defined" sends the
                    // reader off to add a definition that already exists a few lines up.
                    format!(
                        "`{}` is defined on a `roles:` entry in this play, which reaches that \
                         role and the rest of its own entry — not the play's tasks. Move the \
                         value to the play's `vars:` to use it here.",
                        u.name
                    )
                } else {
                    format!(
                        "`{}` is never defined in any file reachable from this playbook — it may \
                         still come from inventory, facts, or extra-vars (-e).",
                        u.name
                    )
                },
                ..Default::default()
            });
        }
        out
    }

    /// Everything startup does after `initialized` has already returned: find the Ansible
    /// install, then scan the workspace.
    ///
    /// Detect runs here rather than on the pump (T-084) because its slow path shells out to
    /// `ansible --version`, which is seconds when Python's import cache is cold — the freeze
    /// T-075's scan fix left behind, and one the scan's own metrics never counted. Detect
    /// still goes first: the scan's module resolution wants the install anyway.
    async fn startup(state: Arc<State>, client: Client) {
        // Read before the move: the setting lives on `state`, and the blocking task must not
        // borrow it.
        let ansible_path = state.ansible_path();
        let install = tokio::task::spawn_blocking(move || {
            ansible_core::install::AnsibleInstall::detect(ansible_path)
        })
        .await
        .unwrap_or_default();
        client
            .log_message(
                MessageType::INFO,
                format!(
                    "ansible-lsp detect: {}{} in {:.0} ms{}",
                    install.source.as_str(),
                    install
                        .version
                        .map(|v| format!(" (core {v})"))
                        .unwrap_or_default(),
                    install.detect_ms,
                    install
                        .package_dir
                        .as_ref()
                        .map(|p| format!(" — {}", p.display()))
                        .unwrap_or_default()
                ),
            )
            .await;
        // No install means builtins and installed collections can't resolve — say so, so a
        // plain `ansible.builtin.debug` that won't jump reads as "no Ansible here", not "the
        // tool is broken". In-repo files, roles, and modules still work. The status
        // notification drives a persistent status-bar item; the toast is the immediate nudge.
        let found = install.package_dir.is_some();
        let _ = client
            .send_notification::<AnsibleStatus>(serde_json::json!({ "found": found }))
            .await;
        if !found {
            client
                .show_message(
                    MessageType::WARNING,
                    "Ansible not found on PATH — builtin modules (ansible.builtin.*) and \
                     installed collections won't resolve. In-repo files, roles, and modules \
                     still work. Install ansible-core (WSL on Windows).",
                )
                .await;
        }
        Self::publish_inventory(&state, &client).await;
        Self::scan_workspace(state, client).await;
    }

    /// Tell the client which inventory is in effect and how it was chosen, so the status bar
    /// can show it.
    ///
    /// Delivery only — every decision is in [`inventory_status`](Self::inventory_status), the
    /// same split `unparseable_diagnostic`/`_for` and `diagnostics_of`/`_with` already use. It
    /// is what makes the answer testable: the payload is a value a test can assert on, where a
    /// notification is only observable by holding and draining the client socket. What stays
    /// untested here is the two lines that hand that value to `client`.
    async fn publish_inventory(state: &Arc<State>, client: &Client) {
        let status = Self::inventory_status(state);
        client
            .log_message(
                MessageType::INFO,
                format!(
                    "ansible-lsp inventory: source={} resolved={} declined={} candidates={}",
                    status["source"], status["resolved"], status["declined"], status["candidates"]
                ),
            )
            .await;
        let _ = client.send_notification::<InventoryStatus>(status).await;
    }

    /// Which inventory is in effect and how it was chosen — the whole of the status bar's
    /// answer, as a value. `source` is what the user needs to reason about a surprise:
    /// "setting" means they picked it, anything else means we followed Ansible's own ladder.
    fn inventory_status(state: &Arc<State>) -> serde_json::Value {
        let configured = state.inventory_setting();
        let root = state.roots.lock().ok().and_then(|r| r.first().cloned());
        // Which rung of Ansible's ladder actually answered. `config.rs` collapses the env
        // var and the file into one field — correct for reading, but the user needs to know
        // *why* a given inventory is in effect before they can argue with it.
        // Which `ansible.cfg`, not just "ansible.cfg". Ansible reads the one in the
        // directory you run from — measured: no walk up a parent, and a second config
        // never merges, the nearer file wins whole — so a repo with more than one has an
        // answer that depends on your cwd, which an editor cannot see. Naming the file we
        // read is what makes that visible instead of a silent mismatch.
        let (cfg_names_one, config_file) = root
            .as_deref()
            .map(|r| {
                let ctx = ScanCache::default().context(&r.join("x.yml"));
                let named = ctx.config.inventory.is_some();
                let file = ctx
                    .config
                    .config_file
                    .as_ref()
                    .map(|f| f.strip_prefix(r).unwrap_or(f).display().to_string());
                (named, file)
            })
            .unwrap_or((false, None));
        let source = if !configured.is_empty() {
            "ansibleLsp.inventory"
        } else if std::env::var("ANSIBLE_INVENTORY").is_ok_and(|v| !v.trim().is_empty()) {
            "ANSIBLE_INVENTORY"
        } else if cfg_names_one {
            "ansible.cfg"
        } else {
            "/etc/ansible/hosts"
        };
        let resolve = |override_paths: Vec<PathBuf>| -> Vec<String> {
            root.as_deref()
                .map(|r| {
                    let cache = ScanCache::default().with_inventory(override_paths);
                    ansible_core::inventory::sources(
                        &cache.context(&r.join("x.yml")).config,
                        &cache,
                    )
                    .iter()
                    .map(|p| p.strip_prefix(r).unwrap_or(p).display().to_string())
                    .collect()
                })
                .unwrap_or_default()
        };
        let resolved = resolve(configured.clone());
        // What a plain `ansible-playbook` would read here, with the editor's stand-in for
        // `-i` taken away. The picker's "nothing chosen" state says which rung it would
        // follow; naming the rung is not the useful half, since `/etc/ansible/hosts`
        // usually does not exist and the honest answer is that nothing resolves at all.
        let auto_source = if std::env::var("ANSIBLE_INVENTORY").is_ok_and(|v| !v.trim().is_empty())
        {
            "ANSIBLE_INVENTORY"
        } else if cfg_names_one {
            "ansible.cfg"
        } else {
            "/etc/ansible/hosts"
        };
        let auto_resolved =
            if configured.is_empty() { resolved.clone() } else { resolve(Vec::new()) };
        // Which of the resolved sources we detected as dynamic and did not run. Sent apart
        // from `resolved` because "this file is in effect" and "we read this file" are two
        // claims, and showing only the first is what let the picker look omniscient about a
        // host list it never saw. A declined source is in effect *and* unread.
        let declined: Vec<String> = root
            .as_deref()
            .map(|r| {
                let cache = ScanCache::default().with_inventory(configured.clone());
                ansible_core::vars::declined_inventories(&r.join("x.yml"), &cache)
                    .iter()
                    .map(|p| p.strip_prefix(r).unwrap_or(p).display().to_string())
                    .collect()
            })
            .unwrap_or_default();
        let candidates = Self::inventory_candidates(root.as_deref());
        serde_json::json!({
            "source": source,
            "resolved": resolved,
            "declined": declined,
            "autoSource": auto_source,
            "autoResolved": auto_resolved,
            "configFile": config_file,
            "candidates": candidates,
        })
    }

    /// What the picker offers: inventory files anywhere in the workspace, and the
    /// **directories** that hold them — `-i prod/` is as valid as `-i prod/hosts.ini`, and
    /// it is the spelling a repo with `prod/db.ini` + `prod/hosts.ini` actually wants.
    ///
    /// A directory candidate carries what it expands to, computed by
    /// [`ansible_core::inventory::expand`] — the same function that reads it. The picker
    /// showing one set of files while the reader loads another is the failure this avoids,
    /// and it is not hypothetical: a directory silently drops `.cfg`, `.md` and `.bak`,
    /// which nobody guesses.
    ///
    /// A convenience, never an authority — what is actually read is `resolved` above.
    fn inventory_candidates(root: Option<&Path>) -> Vec<serde_json::Value> {
        let Some(root) = root else { return Vec::new() };
        use ansible_core::fs::Fs as _;
        let fs = ansible_core::fs::StdFs;

        // Sniffing means reading and parsing, so it is spent only where an inventory could
        // plausibly live. The walk itself is the cheap half.
        const PRUNE: &[&str] =
            &[".git", "node_modules", "target", "__pycache__", ".venv", "venv", "dist", "build"];
        const BUDGET: usize = 4000;

        // Forward slashes, not the host separator: these strings are saved into
        // `ansibleLsp.inventory` — which may be the committed, shared setting — shown as
        // `-i <path>`, and split on `/` by the picker to get a file's base name. A Windows
        // `inventories\prod` breaks all three, and reads back fine as `/` on Windows.
        let rel = |p: &Path| {
            p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/")
        };
        let mut files: Vec<PathBuf> = Vec::new();
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut looked = 0usize;

        for (dir, names) in fs.walk(root) {
            // `group_vars`/`host_vars` are pruned by the same rule the reader uses: a
            // directory source steps over them, so offering one as a folder would hand the
            // user a pick that resolves to nothing.
            if dir.components().any(|c| {
                c.as_os_str().to_str().is_some_and(|n| {
                    PRUNE.contains(&n) || ansible_core::inventory::ignored_dir(n)
                })
            }) {
                continue;
            }
            let mut here = 0usize;
            let mut readable = 0usize;
            for name in &names {
                if looked >= BUDGET {
                    break;
                }
                if !ansible_core::inventory::ignored_entry(name) {
                    readable += 1;
                }
                // Every static format we can actually read, plus extension-less, which is
                // legal and common. `.toml` and `.json` were missing while the readers for
                // both existed — offered nothing, so the formats were supported everywhere
                // except the one place you would pick them.
                let ext_ok = matches!(
                    Path::new(name).extension().and_then(|e| e.to_str()),
                    None | Some("yml") | Some("yaml") | Some("ini") | Some("toml") | Some("json")
                );
                if !ext_ok {
                    continue;
                }
                let path = dir.join(name);
                looked += 1;
                let Some(text) = fs.read(&path) else { continue };
                let nodes = ansible_core::parse::Document::new(text.clone()).parse();
                if ansible_core::inventory::looks_like_inventory(
                    &path,
                    &text,
                    nodes.as_deref().unwrap_or(&[]),
                ) {
                    files.push(path);
                    here += 1;
                }
            }
            // Two inventories in one directory is what a directory source is *for*. One is
            // more likely a file that happens to live somewhere, so the folder is not
            // offered and the file still is.
            //
            // The majority clause is what keeps `demo/` out: it holds three inventories
            // among a dozen playbooks, and `-i demo` would hand every one of those
            // playbooks to the inventory parser. A folder is only worth offering when the
            // folder *is* the inventory.
            if here >= 2 && here * 2 > readable {
                dirs.push(dir.clone());
            }
        }

        files.sort();
        dirs.sort();
        let mut out: Vec<serde_json::Value> = Vec::new();
        for d in &dirs {
            // In load order, which for a directory is name order and is not the user's to
            // choose — the picker offers to expand the folder into these files instead,
            // and those it can reorder.
            let reads = ansible_core::inventory::expand(d, &fs);
            out.push(serde_json::json!({
                "path": rel(d),
                "dir": true,
                "reads": reads.iter().map(|p| rel(p)).collect::<Vec<_>>(),
            }));
        }
        // A file already inside an offered folder is not offered again. The folder row
        // lists it, so the second row said nothing and doubled the list — six rows where
        // two carried the whole answer.
        for f in &files {
            if dirs.iter().any(|d| f.starts_with(d)) {
                continue;
            }
            out.push(serde_json::json!({ "path": rel(f), "dir": false }));
        }
        out
    }

    /// Resolve every YAML file in the workspace and publish what's broken.
    ///
    /// Runs as a detached task (T-075): `initialized` spawns this and returns, so hover /
    /// definition / references are serviced while it works, and diagnostics fill the
    /// Problems panel progressively. Each file's analysis is sync CPU+IO, so it runs under
    /// `spawn_blocking` — the async workers stay free too. Because edits now interleave
    /// with the scan, an open buffer wins everywhere: skip at read time, re-check at
    /// publish time, and never clear or overwrite a flag the didChange path owns.
    async fn scan_workspace(state: Arc<State>, client: Client) {
        if state.scanning.swap(true, Ordering::SeqCst) {
            return;
        }
        let scan_start = std::time::Instant::now();
        let roots = state.roots.lock().map(|r| r.clone()).unwrap_or_default();
        let stale: HashSet<Url> = state.flagged.lock().map(|f| f.clone()).unwrap_or_default();
        let mut still_flagged = HashSet::new();
        let mut t = ScanTimings::default();
        let mut seen = 0usize;
        // One cache for the whole pass (T-076): the files share every subtree they reach, so
        // a role's defaults/meta and any shared task file are read, parsed and walked once
        // for all of them instead of once each. Dropped when the scan ends, so it can never
        // outlive the content it was built from.
        // Counting sits *under* the memo, so it reports what actually reached the disk —
        // the walk is ~98% syscalls on a network/9p workspace, so this is the number.
        let disk = Arc::new(Counting::new(StdFs));
        let scan_cache = Arc::new(ScanCache::new(disk.clone()).with_install(state.install()));

        // Analyse several files at once. 580 files: ext4 19 -> 3 ms at `nproc`, 9p 14.5 -> 1.2 s.
        let in_flight = state
            .settings
            .lock()
            .map(|s| s.in_flight())
            .unwrap_or(4)
            .max(1);
        let mut set: tokio::task::JoinSet<Option<(Url, Vec<Diagnostic>, ScanTimings)>> =
            tokio::task::JoinSet::new();

        // Publishing stays here on one task, so `still_flagged` needs no lock and the
        // open-buffer re-check keeps happening immediately before the publish it guards.
        // Order between files carries no meaning: each notification replaces one URI's
        // whole diagnostic set.
        macro_rules! drain_one {
            () => {
                if let Some(done) = set.join_next().await {
                    if let Some((uri, diagnostics, ft)) = done.ok().flatten() {
                        t.add(&ft);
                        if !diagnostics.is_empty() && state.text_of(&uri).is_none() {
                            still_flagged.insert(uri.clone());
                            client.publish_diagnostics(uri, diagnostics, None).await;
                        }
                    }
                }
            };
        }

        let open_docs = state.open_docs();
        for root in roots {
            for path in yaml_files(&root) {
                seen += 1;
                // An open buffer is authoritative over what's on disk.
                let Ok(uri) = Url::from_file_path(&path) else { continue };
                if state.text_of(&uri).is_some() {
                    continue;
                }
                if set.len() >= in_flight {
                    drain_one!();
                }
                let st = state.clone();
                let sc = scan_cache.clone();
                // This file is closed — but a file it reads for variables may be open and
                // edited, so the pass still needs the buffers (T-199).
                let open = open_docs.clone();
                set.spawn_blocking(move || {
                    let mut ft = ScanTimings::default();
                    let text = std::fs::read_to_string(&path).ok()?;
                    let a = Self::analyze_text_measured(text, &path, &mut ft, &sc, &open, &st.var_cache)?;
                    ft.files = 1;
                    let mut diagnostics = Self::diagnostics_of(&a);
                    diagnostics.extend(st.mutated_condition_diagnostics(&a));
                    Some((uri, diagnostics, ft))
                });
            }
        }
        // Every result must land before the cleanup below, which subtracts `still_flagged`
        // from what was flagged previously — an in-flight file missing from that set would
        // have its diagnostics cleared as though it had been fixed.
        while !set.is_empty() {
            drain_one!();
        }

        // Clear files that were flagged before but are clean now. Open buffers are the
        // didChange path's to clear, not ours.
        for uri in stale.difference(&still_flagged) {
            if state.text_of(uri).is_some() {
                continue;
            }
            client.publish_diagnostics(uri.clone(), vec![], None).await;
            if let Ok(mut f) = state.flagged.lock() {
                f.remove(uri);
            }
        }
        // Merge rather than overwrite: handlers flag open files while the scan runs, and
        // a wholesale `*f = still_flagged` would drop those.
        if let Ok(mut f) = state.flagged.lock() {
            f.extend(still_flagged);
        }

        // Startup perf metrics (T-074), to the *Ansible LSP* output channel. Phases are
        // sums across all analysed files; the wall clock also covers the file walk, reads,
        // diagnostics and publishing, so total > parse+context+var-index+resolve.
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
        // The var-walk counters (T-076) are the platform-independent half of this line: on a
        // fast machine the var-index milliseconds can't see the difference, but edges-to-files
        // is the redundancy itself. edges == files means nothing was walked twice.
        let c = scan_cache.stats();
        let f = disk.stats();
        client
            .log_message(
                MessageType::INFO,
                format!(
                    "ansible-lsp scan: {} files analysed of {seen} seen in {:.0} ms \
                     (parse {:.0}, context {:.0}, var-index {:.0}, resolve {:.0}); \
                     var-walk {} edges -> {} files ({} uncached), {} reads, {} contexts, \
                     {} ansible.cfg, {} defs; fs {} syscalls in {:.0} ms ({} missing)",
                    t.files,
                    ms(scan_start.elapsed()),
                    ms(t.parse),
                    ms(t.context),
                    ms(t.var_index),
                    ms(t.resolve),
                    c.edges,
                    c.files,
                    c.uncached,
                    c.reads,
                    c.contexts,
                    c.configs,
                    c.defs,
                    f.calls(),
                    f.nanos() as f64 / 1e6,
                    f.misses(),
                ),
            )
            .await;
        state.scanning.store(false, Ordering::SeqCst);
    }

    /// Every resolvable reference and how many files it reaches.
    async fn resolved_references(&self, p: ReferencesParams) -> Result<Vec<ResolvedRef>> {
        let Some(a) = self.state.analyze(&p.uri) else {
            return Ok(Vec::new());
        };
        let mut out: Vec<ResolvedRef> = a
            .refs
            .iter()
            .filter(|(_, res)| res.status == Status::Resolved)
            // A vars_files group anchor spans the whole nested list; painting it would
            // double-decorate the winning alternative underneath.
            .filter(|(r, _)| r.vars_files_group.is_none())
            .map(|(r, res)| {
                let (sl, sc) = a.doc.byte_to_lsp(r.span.start);
                let (el, ec) = a.doc.byte_to_lsp(r.span.end);
                ResolvedRef {
                    range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
                    targets: res.targets.len(),
                    kind: "reference",
                }
            })
            .collect();

        // Variable uses that resolve to a definition — in this file or, following the same
        // deterministic paths as go-to-definition, another one — are clickable too, so paint
        // them. A use with no reachable definition stays plain, matching go-to-definition's
        // silence (it may come from inventory or a caller). This walks includes/roles per
        // repaint; it's debounced and depth-capped, and can be cached if it ever lags.
        if let Ok(path) = p.uri.to_file_path() {
            let nodes = &a.nodes;
            let all = cached_definitions(
                &path,
                nodes,
                &self.state.open_docs(),
                &self.state.inventory_setting(),
                &self.state.var_cache,
                self.state.install().as_ref(),
            );
            for u in vars::uses(nodes) {
                // In-effect count depends on the use position (a later set_fact hasn't run),
                // so it's computed per use rather than once per name.
                let n = all
                    .iter()
                    .filter(|d| d.name == u.name && d.in_effect_for(&u, &path))
                    .count();
                if n == 0 {
                    continue;
                }
                let (sl, sc) = a.doc.byte_to_lsp(u.span.start);
                let (el, ec) = a.doc.byte_to_lsp(u.span.end);
                out.push(ResolvedRef {
                    range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
                    targets: n,
                    kind: "variable",
                });
            }
        }
        Ok(out)
    }

    /// What Cmd+click resolves to, in the order the three kinds are tried: a file/role
    /// reference, then a variable use, then the host half of a `hostvars['web01']` read.
    /// Split out of the trait method so a test covers the *chain* — which of the three
    /// answers, and that the later ones cannot steal a click from the earlier ones.
    fn definition_at(
        doc: &Document,
        nodes: &[Node],
        pos: Position,
        uri: &Url,
        path: &Path,
        open: &OpenDocs,
        inv: &[PathBuf],
        cache: &Mutex<VarCache>,
        install: Option<&Arc<AnsibleInstall>>,
    ) -> Option<Vec<Location>> {
        let Some((reference, in_playbook)) = Self::reference_at(doc, nodes, pos) else {
            // Not on a file/role/module reference — maybe on a variable use. Jump to where
            // it's defined in this file (cross-file sources are a later step).
            return Self::variable_defs_at(doc, nodes, pos, uri, open, inv, cache, install)
                // Or on the host half of a `hostvars['web01']` read (T-171).
                .or_else(|| Self::host_key_defs_at(doc, pos, path));
        };
        let ctx = FileContext::discover(path).with_install(install.cloned());
        let res = resolve::Resolver { in_playbook, ..Default::default() }
            .resolve(&reference, &ctx);
        if res.status != Status::Resolved {
            return None;
        }
        let locations: Vec<Location> = res.targets.iter().filter_map(|t| location_at(t)).collect();
        (!locations.is_empty()).then_some(locations)
    }

    /// The reference under the cursor, and whether its file is a playbook — the resolver
    /// needs the second to know what `{{ playbook_dir }}` means, and it is a fact about the
    /// file that no single reference carries any more (T-135).
    fn reference_at(
        doc: &Document,
        nodes: &[Node],
        pos: Position,
    ) -> Option<(Reference, bool)> {
        let byte = doc.lsp_to_byte(pos.line, pos.character);
        let extracted = references::extract(nodes);
        let in_playbook = extracted.in_playbook;
        extracted
            .refs
            .into_iter()
            .find(|r| r.span.start <= byte && byte <= r.span.end)
            .map(|r| (r, in_playbook))
    }

    /// T-171: the *host* half of `hostvars['web01'].x`. `host_vars/web01.yml` beside the
    /// playbook is a deterministic path — the filename is the host name — so this needs no
    /// inventory. What groups the host belongs to, and everything else a host "is", stays
    /// T-062's; answering only "where are this host's variables written" is what keeps the
    /// claim true without one.
    fn host_key_defs_at(doc: &Document, pos: Position, path: &Path) -> Option<Vec<Location>> {
        let byte = doc.lsp_to_byte(pos.line, pos.character);
        let (host, _, _) = condition::hostvars_host_key_at(&doc.text, byte)?;
        location_at(&host_vars_file(&host, path)?).map(|l| vec![l])
    }

    /// Every host key in the file that resolves, as a paintable link.
    fn host_key_links(doc: &Document, path: &Path) -> Vec<DocumentLink> {
        condition::hostvars_host_keys(&doc.text)
            .into_iter()
            .filter_map(|(host, s, e)| {
                let target = host_vars_file(&host, path)?;
                let (sl, sc) = doc.byte_to_lsp(s);
                let (el, ec) = doc.byte_to_lsp(e);
                Some(DocumentLink {
                    range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
                    target: Some(Url::from_file_path(&target).ok()?),
                    tooltip: Some(target.display().to_string()),
                    data: None,
                })
            })
            .collect()
    }

    /// If `pos` sits on a variable use, the location(s) where that variable is defined in
    /// the same file. `None` when the cursor isn't on a use, or the name has no in-file
    /// definition — absence here is not proof it's undefined (inventory, role defaults and
    /// `vars_files` aren't indexed yet), so we stay silent rather than guess.
    fn variable_defs_at(
        doc: &Document,
        nodes: &[Node],
        pos: Position,
        uri: &Url,
        open: &OpenDocs,
        inv: &[PathBuf],
        cache: &Mutex<VarCache>,
        install: Option<&Arc<AnsibleInstall>>,
    ) -> Option<Vec<Location>> {
        let byte = doc.lsp_to_byte(pos.line, pos.character);
        // The full view, not the rule-facing one: a definition of `ansible_custom` is a
        // definition, and the prefix must not stop the jump (T-224).
        let use_ = vars::any_uses(nodes)
            .into_iter()
            .find(|u| byte >= u.span.start && byte < u.span.end)?;
        let path = uri.to_file_path().ok()?;
        let defs: Vec<vars::Located> = cached_definitions(&path, nodes, open, inv, cache, install)
            .iter()
            .filter(|d| d.name == use_.name && d.in_effect_for(&use_, &path))
            .cloned()
            .collect();
        // Jump to the definition that actually applies here — highest precedence, latest on a
        // tie — rather than a picker of every assignment. (hover lists them all, ranked.)
        let d = vars::effective(&defs)?;
        located_at(d, &path, &doc.text, open).map(|l| vec![l])
    }

    /// Markdown for the variable under the cursor: each place it's defined (source, file and
    /// value), plus the range of the use to anchor the hover. `None` if the cursor isn't on a
    /// variable use, or the name has no reachable definition.
    /// Hover for a templated reference path (`{{ env }}.yml`) that was resolved by
    /// substituting known-value variables: show what it resolves to, and for each variable
    /// its value and where that value is defined — so "why does this go to prod.yml" is
    /// answered in place.
    fn path_substitution_hover(
        doc: &Document,
        nodes: &[Node],
        r: &Reference,
        res: &Resolution,
        path: &Path,
        open: &OpenDocs,
        inv: &[PathBuf],
        cache: &Mutex<VarCache>,
        install: Option<&Arc<AnsibleInstall>>,
    ) -> Option<(String, Range)> {
        let idents = template_idents(&r.value);
        if idents.is_empty() {
            return None;
        }
        let defs = cached_definitions(path, nodes, open, inv, cache, install);
        let mut ext: HashMap<PathBuf, Document> = HashMap::new();
        let mut lines = Vec::new();
        for token in idents {
            let cands: Vec<vars::Located> =
                defs.iter().filter(|d| d.name == token).cloned().collect();
            let Some(d) = vars::effective(&cands) else {
                continue;
            };
            let document: &Document = if d.file == *path {
                doc
            } else {
                ext.entry(d.file.clone()).or_insert_with(|| {
                    Document::new(open.read(&d.file).unwrap_or_default())
                })
            };
            let text = document.text.as_str();
            let Some(v) = def_value(d, text) else {
                continue;
            };
            let line = document.line_of(d.span.start);
            lines.push(
                md::code(&token)
                    + " = "
                    + md::code(&v)
                    + " — "
                    + source_label(d.source).md()
                    + " · "
                    + def_link(&d.file, line),
            );
        }
        if lines.is_empty() {
            return None;
        }
        let mut out = Md::new();
        if !res.targets.is_empty() {
            let t = res
                .targets
                .iter()
                .map(|p| short_path(p))
                .collect::<Vec<_>>()
                .join(", ");
            out = out.line(("→ ".md() + md::code(&t)).bold());
        }
        let out = out.line("Substituting:".md()).items(lines);
        let (sl, sc) = doc.byte_to_lsp(r.span.start);
        let (el, ec) = doc.byte_to_lsp(r.span.end);
        Some((out.render(), Range::new(Position::new(sl, sc), Position::new(el, ec))))
    }

    /// Hover for a name Ansible injects. Separate from the variable hover because the two
    /// answer different questions: that one says where a definition is, and for these there
    /// is none anywhere — which is the expected state, not a finding.
    ///
    /// The install is passed in, not fetched: the caller supplies
    /// [`AnsibleInstall::detected`] — never `detect`, since a hover must not be what starts
    /// detection (T-084's 3.6 s freeze) — and a test supplies a synthetic one, which is the
    /// only way to exercise this path on a machine with no Ansible.
    fn injected_var_hover_at(
        doc: &Document,
        use_: &vars::VarUse,
        install: Option<&AnsibleInstall>,
    ) -> Option<(String, Range)> {
        let md = injected_var_hover(&use_.name, install)?;
        let (sl, sc) = doc.byte_to_lsp(use_.span.start);
        let (el, ec) = doc.byte_to_lsp(use_.span.end);
        Some((md, Range::new(Position::new(sl, sc), Position::new(el, ec))))
    }

    fn variable_hover_at(
        doc: &Document,
        nodes: &[Node],
        byte: usize,
        path: &Path,
        open: &OpenDocs,
        inv: &[PathBuf],
        cache: &Mutex<VarCache>,
        install: Option<&Arc<AnsibleInstall>>,
    ) -> Option<(String, Range)> {
        // One scan for both kinds of name. The rule-facing `vars::uses` drops the injected
        // ones, so asking it first and falling back to a second, complementary scan walked
        // the whole tree twice for every token that is not an ordinary variable.
        let use_ = vars::any_uses(nodes)
            .into_iter()
            .find(|u| byte >= u.span.start && byte < u.span.end)?;
        let mut defs: Vec<vars::Located> = cached_definitions(path, nodes, open, inv, cache, install)
            .iter()
            .filter(|d| d.name == use_.name && d.in_effect_for(&use_, path))
            .cloned()
            .collect();
        // A definition answers first. Only a name nothing defines falls back to what ansible
        // provides — the other way round hid a user's own `ansible_custom` behind its
        // prefix (T-224).
        if defs.is_empty() {
            return Self::injected_var_hover_at(doc, &use_, install.map(|i| i.as_ref()));
        }
        // Highest precedence first (latest on a tie): defs[0] is what actually applies here.
        defs.sort_by(|a, b| {
            b.source
                .precedence()
                .cmp(&a.source.precedence())
                .then(b.span.start.cmp(&a.span.start))
        });
        let multiple = defs.len() > 1;
        let mut ext: HashMap<PathBuf, Document> = HashMap::new();
        let mut lines = Vec::new();
        for (i, d) in defs.iter().enumerate() {
            // Provenance chain (T-066): how a def from a role never named in this file got
            // into scope — one nested line per meta/main.yml dependency hop, innermost
            // first like a stack trace (the defining role's requirer at the top). Only
            // non-obvious routes carry `via`; direct roles/includes stay bare. Unbounded
            // like Ansible's own dep_chain — length is capped by the visited set.
            let via_lines: Vec<md::Inline> = d
                .via
                .iter()
                .rev()
                .map(|(vf, vs)| {
                    let vdoc = ext.entry(vf.clone()).or_insert_with(|| {
                        Document::new(open.read(vf).unwrap_or_default())
                    });
                    let vline = vdoc.line_of(vs.start);
                    // The requirer is the role owning the meta file: roles/<role>/meta/…
                    let role = vf
                        .parent()
                        .and_then(|m| m.parent())
                        .and_then(|r| r.file_name())
                        .and_then(|s| s.to_str())
                        .unwrap_or("?");
                    "dependency of ".md() + md::code(role) + " — " + def_link(vf, vline)
                })
                .collect();
            let document: &Document = if d.file == *path {
                doc
            } else {
                ext.entry(d.file.clone()).or_insert_with(|| {
                    Document::new(open.read(&d.file).unwrap_or_default())
                })
            };
            let text = document.text.as_str();
            let line = document.line_of(d.span.start);
            let mark = if multiple && i == 0 { "  ← effective".md() } else { md::empty() };
            // A conditionally-defined var (task under a `when:`) reads as "only when …" —
            // that's how "web01 but not web02" shows up without any inventory.
            let cond = match &d.condition {
                Some(c) => {
                    " ".md() + ("(only when ".md() + md::code(c.trim()) + ")").italic()
                }
                None => md::empty(),
            };
            let head = source_label(d.source).md() + " · " + def_link(&d.file, line);
            let head = match def_value(d, text) {
                Some(v) => head + " = " + md::code(&v),
                None => head,
            };
            lines.push((false, head + cond + mark));
            lines.extend(via_lines.into_iter().map(|v| (true, v)));
        }
        let name = md::code(&use_.name).bold();
        let header = if multiple {
            name + " — " + md::text(&defs.len().to_string()) + " definitions"
        } else {
            name
        };
        let mut md = lines
            .into_iter()
            .fold(Md::new().line(header).gap(), |doc, (nested, l)| {
                if nested { doc.subitem(l) } else { doc.item(l) }
            });
        // A host-scoped winner (group_vars/<group>, host_vars/<host>) is only in effect for
        // matching hosts — we can't verify that without inventory. A role-default winner can
        // still be overridden by inventory we don't index. Otherwise only `-e` can.
        let caveat = if defs[0].source.host_scoped() {
            Some("Host-scoped: in effect only for matching hosts; `-e` can override.")
        } else if defs[0].source == vars::VarSource::RoleDefaults {
            Some("Inventory (per-host) or `-e` can still override.")
        } else if multiple {
            Some("`-e` extra-vars can still override.")
        } else {
            None
        };
        if let Some(c) = caveat {
            md = md.line(c.italic());
        }
        let (sl, sc) = doc.byte_to_lsp(use_.span.start);
        let (el, ec) = doc.byte_to_lsp(use_.span.end);
        Some((md.render(), Range::new(Position::new(sl, sc), Position::new(el, ec))))
    }
}

/// The `host_vars/<host>.yml` file beside `from`, if it exists (T-171). A deterministic
/// path — the filename is the host name — so it needs no inventory. Which *groups* the host
/// is in is a different question and waits for T-062.
fn host_vars_file(host: &str, from: &Path) -> Option<PathBuf> {
    let dir = from.parent()?.join("host_vars");
    ["yml", "yaml"]
        .iter()
        .map(|ext| dir.join(format!("{host}.{ext}")))
        .find(|p| p.is_file())
}

/// Human label for a variable's definition source.
fn source_label(s: vars::VarSource) -> &'static str {
    use vars::VarSource::*;
    match s {
        PlayVars => "play var",
        BlockVars => "block var",
        TaskVars => "task var",
        SetFact => "set_fact",
        Register => "register",
        VarsFiles => "vars_files",
        RoleDefaults => "role default",
        RoleVars => "role var",
        GroupVarsAll => "group_vars/all",
        GroupVars => "group_vars",
        HostVars => "host_vars",
        Inventory => "inventory",
        IncludeVars => "include_vars",
        RoleParams => "role param",
        RoleEntryVars => "roles: entry vars",
        AddHost => "add_host",
    }
}

/// The literal value of a definition, when its span points at a value (play/block/task vars,
/// vars_files, role defaults/vars, `add_host` args). `set_fact`/`register` spans point at the
/// name, so those carry no value here. Whitespace-collapsed and length-capped for a one-line
/// hover.
fn def_value(d: &vars::Located, text: &str) -> Option<String> {
    use vars::VarSource::*;
    match d.source {
        PlayVars | BlockVars | TaskVars | VarsFiles | RoleDefaults | RoleVars
        | GroupVarsAll | GroupVars | HostVars | Inventory | IncludeVars | RoleParams
        | RoleEntryVars | AddHost => {
            let raw = d.span.slice(text).trim();
            if raw.is_empty() {
                return None;
            }
            let one = raw.split_whitespace().collect::<Vec<_>>().join(" ");
            Some(if one.chars().count() > 60 {
                format!("{}…", one.chars().take(60).collect::<String>())
            } else {
                one
            })
        }
        SetFact | Register => None,
    }
}

/// The bare-identifier `{{ tokens }}` in a string (a filter or expression is not one).
fn template_idents(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = value;
    while let Some(o) = rest.find("{{") {
        let after = &rest[o + 2..];
        let Some(c) = after.find("}}") else { break };
        let tok = after[..c].trim();
        if !tok.is_empty() && tok.chars().all(|ch| ch.is_alphanumeric() || ch == '_') {
            out.push(tok.to_string());
        }
        rest = &after[c + 2..];
    }
    out
}

/// The last few components of a path, for a compact "where it's defined" label.
/// A definition's location as a clickable `file:line`, falling back to a code span when the
/// path can't be expressed as a URL. Both variable hovers and the provenance chain want this.
fn def_link(file: &Path, line: usize) -> md::Inline {
    let label = format!("{}:{line}", short_path(file));
    match Url::from_file_path(file) {
        Ok(u) => md::link(&label, &u, Some(line)),
        Err(()) => md::code(&label),
    }
}

fn short_path(p: &Path) -> String {
    let comps: Vec<_> = p.components().collect();
    let start = comps.len().saturating_sub(3);
    comps[start..]
        .iter()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Hover markdown for a conditional reference: every clause spelled out in plain English
/// (or verbatim when it doesn't match a known shape), plus the reminder that an import's
/// `when:` fans out onto the whole imported file.
fn when_hover(r: &Reference) -> String {
    let doc = Md::new().line(md::code("when:").bold());
    let doc = if let [only] = r.conditions.as_slice() {
        // A single clause is the whole condition, so keep the "runs unless / only if"
        // framing that says what it decides.
        doc.line(labelled(only))
    } else {
        // Listed clauses are ANDed. Say so, and state each as a bare requirement rather
        // than as its own "runs only if" sentence, which would read as standalone.
        doc.line("Runs only when ".md() + "all".bold() + " hold:")
            .items(r.conditions.iter().map(|c| required(c)))
    };
    if r.kind == ReferenceKind::ImportPlaybook {
        return doc
            .line(
                "The condition is copied onto every task in the imported playbook and \
                 re-checked per task."
                    .italic(),
            )
            .render();
    }
    doc.render()
}

/// One clause as a whole-condition sentence, or the expression itself when no shape
/// matches — which is the common case, and the reason the value needs a code span that
/// survives a backtick inside it.
fn labelled(c: &str) -> md::Inline {
    condition::classify(c)
        .label()
        .map(|l| md::text(&l))
        .unwrap_or_else(|| md::code(c.trim()))
}

/// One clause as a bare requirement, for ANDing with its siblings.
fn required(c: &str) -> md::Inline {
    condition::classify(c)
        .requirement()
        .map(|l| md::text(&l))
        .unwrap_or_else(|| md::code(c.trim()))
}

/// A path as it appears in a tooltip: project-relative, forward slashes, code span. Every
/// hover use of [`shorten`] is this, so the pairing is one call rather than two.
fn path_span(p: &Path, ctx: &FileContext) -> md::Inline {
    md::code(&shorten(p, ctx))
}

/// A **diagnostic** message, not a hover — LSP defines `Diagnostic.message` as plain text,
/// so nothing here is markdown and `md` deliberately stays out of it. The backticks are a
/// quoting convention the editor shows literally. (T-082 lists this among the hover
/// producers; it isn't one.)
fn message_for(r: &Reference, res: &Resolution, ctx: &FileContext) -> String {
    // Listing a candidate path with `{{ }}` still in it explains nothing, and the old
    // wording here ("cannot resolve") was false: with `-e` it resolves and runs. What IS
    // certain is that the file cannot be checked on its own — `--syntax-check` takes no
    // user arguments and exits 4. Say that, name the only two sources that work, and name
    // the escape hatch, because "I do pass -e" is a legitimate answer only the author has.
    // Live-verified against ansible-core 2.21.2; T-095.
    if r.templated && r.kind == ReferenceKind::ImportPlaybook {
        return format!(
            "`{}` is resolved when this file is parsed, before any play or host exists. \
             Only extra-vars (`-e`) or a `vars:` on this line can supply it — play vars, \
             group_vars, host_vars and `set_fact` cannot. So this playbook fails \
             `--syntax-check` and any lint of it, whatever you pass at run time. Supply \
             the value here, split it into one import per case with `when:`, or — if it \
             really does come from `-e` — silence this with `# noqa: templated-import`.",
            r.value
        );
    }
    let tried = res
        .candidates
        .iter()
        .map(|c| format!("  {}", shorten(c, ctx)))
        .collect::<Vec<_>>()
        .join("\n");
    // A directory is the opposite case and must be said first: the lookup found
    // something, tried to read it, and `[Errno 21]` killed the play. Live-verified on
    // 2.21.2 — including that a later candidate which IS a file does not rescue it, which
    // is why the shadowed path is named rather than offered (T-087).
    if let Some(dir) = &res.directory {
        let shadowed = res
            .candidates
            .iter()
            .skip_while(|c| *c != dir)
            .skip(1)
            .find(|c| c.is_file())
            .map(|c| {
                format!(
                    " `{}` is a file and comes later in the search order, but Ansible                      stops at the first candidate that exists, so it is never read.",
                    shorten(c, ctx)
                )
            })
            .unwrap_or_default();
        return format!(
            "`{}` resolves to a directory, `{}`. Ansible opens it and fails with              `[Errno 21] Is a directory`, so the play does not start — this is not a              missing file, which would be skipped silently.{shadowed} Name a file inside              it, or use `include_vars:`, which is the keyword that loads a directory.",
            r.value,
            shorten(dir, ctx)
        );
    }
    // Ansible (2.x) silently skips a missing vars_files file — the play runs, the
    // variables are just never set — so these must not claim the play would fail.
    if let Some(alts) = &r.vars_files_group {
        let names = alts
            .iter()
            .map(|a| format!("`{a}`"))
            .collect::<Vec<_>>()
            .join(", ");
        return format!(
            "none of the alternatives in this `vars_files` list exists: {names}. Ansible \
             silently skips the whole entry, so none of their variables is ever set. \
             Tried:\n{tried}"
        );
    }
    if r.kind == ReferenceKind::VarsFiles {
        return format!(
            "no file found for `{}` — Ansible silently skips a missing `vars_files` \
             entry, so its variables are never set. Tried:\n{tried}",
            r.value
        );
    }
    format!("no file found for `{}`. Tried:\n{tried}", r.value)
}

/// Candidate paths are absolute and long; show them relative to the project root.
fn shorten(p: &Path, ctx: &FileContext) -> String {
    ansible_core::posix_display(
        ctx.project_root
            .as_ref()
            .and_then(|root| p.strip_prefix(root).ok())
            .unwrap_or(p),
    )
}

/// One line of provenance for a resolved module, derived from the winning path's shape:
/// which collection, where it lives (this workspace, an installed collection, or the
/// Ansible install itself), and whether the name hit `plugins/modules/` (ships to the
/// target host) or `plugins/action/` (runs on the controller). The raw path dump the
/// generic hover would show forces reading several 100-char paths to learn these facts.
fn module_hover(r: &Reference, res: &Resolution, ctx: &FileContext) -> Option<Md> {
    let won = res.targets.first()?;
    let s = won.to_string_lossy().replace('\\', "/");
    let in_workspace = ctx.project_root.as_ref().is_some_and(|r| won.starts_with(r));
    let (collection, origin) = match s.find("/ansible_collections/") {
        Some(i) => {
            let mut tail = s[i + "/ansible_collections/".len()..].split('/');
            let coll = format!("{}.{}", tail.next()?, tail.next()?);
            let origin = if in_workspace { "this workspace" } else { "an installed collection" };
            (coll, origin)
        }
        // No collection tree: the builtin package, or a pre-collections legacy `library/`
        // dir — `ansible.legacy` is Ansible's own name for that namespace.
        None => {
            let builtin = ctx
                .install
                .as_ref()
                .and_then(|i| i.package_dir.as_ref())
                .is_some_and(|d| won.starts_with(d));
            if builtin {
                ("ansible.builtin".into(), "the Ansible install")
            } else if in_workspace {
                ("ansible.legacy".into(), "a `library/` dir in this workspace")
            } else {
                ("ansible.legacy".into(), "a legacy library path")
            }
        }
    };
    let is_action = s.contains("/plugins/action/");
    // A same-name action plugin means the task actually runs on the controller. Look in the
    // winner's own tree (install/collection, via the path swap) AND the legacy dirs a role
    // or ansible.cfg can add — the legacy ones take precedence, since a local plugin
    // overrides the module (T-073). Bare name only: those dirs predate namespacing.
    let bare = r.value.rsplit('.').next().unwrap_or(r.value.as_str());
    let twin = if is_action {
        plugin_twin(won, is_action).filter(|p| p.is_file())
    } else {
        legacy_action_twin(bare, ctx).or_else(|| plugin_twin(won, is_action).filter(|p| p.is_file()))
    };
    // Ansible's order is same-name twin, then the platform plugin, then ship it to the
    // host — so this is only consulted once the twin search has come up empty.
    let platform = match (is_action, &twin) {
        (false, None) => network_platform_twin(won, bare, ctx),
        _ => None,
    };
    // One line per file, label first, the path as the link text — which file each link
    // opens must be readable without clicking.
    let entry = |label: &'static str, p: &Path| {
        let shown = short_plugin_path(p, ctx);
        let target = match Url::from_file_path(p) {
            Ok(u) => md::link(&shown, &u, None),
            Err(()) => md::code(&shown),
        };
        label.md() + ": " + target
    };
    // Lead with the fact that matters: where this task's code runs. An action plugin —
    // the winner itself or a same-name twin — executes on the controller; otherwise the
    // `normal` handler ships the module to the target host.
    let mut doc = Md::new().line(
        // `origin` is authored prose and one arm carries its own code span
        // (``a `library/` dir…``), so it is markdown already, not a value to escape.
        md::code(&collection)
            + " — from "
            + origin.md()
            + " · "
            + if is_action || twin.is_some() || platform.is_some() {
                "runs on the controller (action plugin)".md()
            } else {
                "runs on the target host".md()
            },
    );
    // Why a module with no twin of its own still runs on the controller. Without this line
    // the platform plugin's filename (`ios.py` for an `ios_config:` task) looks like a
    // mismatch rather than the whole point.
    if let Some(p) = &platform {
        let prefix = p.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
        doc = doc.line(
            "handled by the ".md()
                + md::code(&prefix)
                + " platform plugin, which serves every "
                + md::code(&format!("{prefix}_*"))
                + " module in this collection",
        );
    }
    // A winner whose final name differs from what the task wrote got there through a
    // rename table (core's 2.10 split table, or a collection's own). Make the hop
    // visible: it is otherwise an unmarked seam in the Tried list.
    if s.contains("/ansible_collections/") {
        let stem = won.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
        let resolved_as = format!("{collection}.{stem}");
        if resolved_as != r.value {
            doc = doc.line(
                md::code(&r.value) + " → redirected to " + md::code(&resolved_as),
            );
        }
    }
    // The file that runs is listed first.
    Some(match (is_action, &twin) {
        (true, Some(t)) => doc.item(entry("action plugin", won)).item(entry("module", t)),
        (true, None) => doc.item(entry("action plugin", won)),
        (false, Some(t)) => doc.item(entry("action plugin", t)).item(entry("module", won)),
        (false, None) => match &platform {
            Some(p) => doc.item(entry("platform action plugin", p)).item(entry("module", won)),
            None => doc.item(entry("module", won)),
        },
    })
}

/// A module path cut to its meaningful tail: project-relative inside the workspace,
/// after `site-packages/` for an install, `~`-relative under the home dir. The full
/// path stays one click away in the link itself.
fn short_plugin_path(p: &Path, ctx: &FileContext) -> String {
    if let Some(root) = &ctx.project_root {
        if let Ok(rel) = p.strip_prefix(root) {
            return ansible_core::posix_display(rel);
        }
    }
    // POSIX-shaped from here on: the `/site-packages/` and home-dir cuts below are searches
    // for separators, so on Windows they'd never match a `\` path.
    let s = ansible_core::posix_display(p);
    // The leading `…/` marks a truncated absolute path — without it the tail reads
    // like a workspace-relative one (the repo really has collections/ansible_collections/).
    if let Some(i) = s.find("/site-packages/") {
        return format!("…/{}", &s[i + "/site-packages/".len()..]);
    }
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = s.strip_prefix(home.as_str()) {
            return format!("~{rest}");
        }
    }
    s
}

/// The sibling of a winning module path: its `plugins/action/` twin, or for an action
/// winner the `plugins/modules/` file, in either layout.
/// A controller-side action plugin that legacy-overrides a same-named module, found in a
/// role's `action_plugins/` or a cfg `action_plugins` dir (T-073). Bare name only — those
/// pre-collections dirs predate namespacing. First hit in Ansible's search order wins.
fn legacy_action_twin(name: &str, ctx: &FileContext) -> Option<PathBuf> {
    let file = format!("{name}.py");
    ctx.legacy_action_plugin_dirs()
        .into_iter()
        .map(|d| d.join(&file))
        .find(|p| p.is_file())
}

/// The platform action plugin handling a whole `<prefix>_*` network family, for a module
/// with no same-name twin. Network collections ship one plugin per *platform* rather than
/// per module — it owns the persistent device connection, and a switch has no Python to
/// receive a module — so the task runs on the controller regardless. (T-072)
///
/// `task_executor.py:939-947,961-962` requires **both** halves: the prefix must be a
/// configured platform *and* the plugin must exist. Testing only for the file mislabels any
/// non-network `<x>_*` module that happens to have an `<x>.py` beside it.
fn network_platform_twin(won: &Path, bare: &str, ctx: &FileContext) -> Option<PathBuf> {
    let prefix = bare.split('_').next()?;
    if !ctx.config.is_network_platform(prefix) {
        return None;
    }
    // Ansible looks up `<module collection>.<prefix>`, so the plugin is the winner's own
    // collection sibling: `<coll>/plugins/modules/ios_config.py` -> `<coll>/plugins/action/
    // ios.py`. Matched on path *components*, not by string replace — the winner arrives
    // with native separators, so a `/plugins/modules/` substring search finds nothing on
    // Windows.
    let dir = won.parent()?;
    let named = |p: Option<&Path>, want| p.and_then(Path::file_name).and_then(|s| s.to_str()) == Some(want);
    if named(Some(dir), "modules") && named(dir.parent(), "plugins") {
        let p = dir.with_file_name("action").join(format!("{prefix}.py"));
        return p.is_file().then_some(p);
    }
    // Not a collection layout. Upstream drops the namespace here and looks for a bare
    // `<prefix>`, which is the pre-collections search (T-073). The core package's own
    // `ansible/modules/` never lands here — it ships no network modules.
    legacy_action_twin(prefix, ctx)
}

fn plugin_twin(won: &Path, is_action: bool) -> Option<PathBuf> {
    // Matched on path *components*, not by string replace — `won` arrives with native
    // separators, so a `/plugins/modules/` substring search finds nothing on Windows.
    let (own_kind, twin_kind) = if is_action { ("action", "modules") } else { ("modules", "action") };
    let own_dir = nearest_ancestor_named(won, own_kind)?;
    let file = won.strip_prefix(own_dir).ok()?;
    // Collection/install layout: `.../plugins/modules/x.py` <-> `.../plugins/action/x.py`.
    if is_named(own_dir.parent(), "plugins") {
        return Some(own_dir.with_file_name(twin_kind).join(file));
    }
    // Core modules sit outside plugins/: `.../ansible/modules/x.py` ->
    // `.../ansible/plugins/action/x.py`. An action winner has no such second shape.
    (!is_action).then(|| own_dir.with_file_name("plugins").join("action").join(file))
}

/// The closest directory above `file` carrying this name — the `modules/` a module sits
/// under, even one subdir deeper in a collection. None: no such ancestor.
fn nearest_ancestor_named<'a>(file: &'a Path, name: &str) -> Option<&'a Path> {
    let mut dir = file.parent()?;
    while !is_named(Some(dir), name) {
        dir = dir.parent()?;
    }
    Some(dir)
}

fn is_named(dir: Option<&Path>, name: &str) -> bool {
    dir.and_then(Path::file_name).is_some_and(|n| n == name)
}

/// Every candidate tried, in order, marking the one that won.
fn tried_list(res: &Resolution, ctx: &FileContext) -> Md {
    let won = res.targets.first();
    Md::new()
        .line("Tried:".bold())
        .items(res.candidates.iter().map(|c| {
            if Some(c) == won {
                "✓ ".md() + path_span(c, ctx) + " — won"
            } else {
                path_span(c, ctx)
            }
        }))
}

/// One compact line saying a reference is guarded, to sit under whatever the reference
/// hover already says. The full `when:` block belongs on the `when:` clause; here the guard
/// is a property of the edge — "this include may not be taken" — so it must not crowd out
/// the target or the provenance it appends to.
fn guard_line(r: &Reference) -> Md {
    let doc = match r.conditions.as_slice() {
        [only] => Md::new().line("Conditional".italic() + " : " + labelled(only)),
        many => Md::new()
            .line("Conditional".italic() + " — runs only when " + "all".bold() + " hold:")
            .items(many.iter().map(|c| required(c))),
    };
    if r.kind == ReferenceKind::ImportPlaybook {
        return doc.line("…and copied onto every task in the imported playbook.".italic());
    }
    doc
}

/// Where a resolved reference points, in one line. What the verbose `Tried:` dump says
/// implicitly with a ✓, for the case where the dump isn't wanted but something has to
/// anchor the guard line above.
fn target_line(res: &Resolution, ctx: &FileContext) -> Option<Md> {
    let t = res.targets.first()?;
    Some(Md::new().line(("→ ".md() + path_span(t, ctx)).bold()))
}

/// Hover markdown for a reference: the resolved target and every candidate tried, in
/// order, marking the one that won. Modules lead with a provenance line linking both the
/// module and its action plugin; the path dump follows only when `show_tried` (the
/// `candidatesOnResolved` setting) asks for it. Missing references return `None` — their
/// diagnostic already carries the tried list, and VS Code renders diagnostics in the same
/// tooltip, so a hover would print it twice.
fn reference_hover(
    r: &Reference,
    res: &Resolution,
    ctx: &FileContext,
    show_tried: bool,
) -> Option<Md> {
    if r.kind == ReferenceKind::Module && res.status == Status::Resolved {
        let doc = module_hover(r, res, ctx)?;
        return Some(if show_tried {
            doc.concat(tried_list(res, ctx))
        } else {
            doc
        });
    }
    match res.status {
        // A templated pattern that glob-matched: every target is equally possible until
        // runtime, so there is no winner to mark — list them all.
        Status::Resolved if res.skip_reason == Some(SkipReason::Templated) => {
            let n = res.targets.len();
            let plural = if n == 1 { " possible target:" } else { " possible targets:" };
            Some(
                Md::new()
                    .line((md::text(&n.to_string()) + plural).bold())
                    .items(res.targets.iter().map(|t| path_span(t, ctx))),
            )
        }
        Status::Resolved => Some(tried_list(res, ctx)),
        Status::Missing => None,
        Status::Skipped => match res.skip_reason {
            // No candidates means no known-value substitution happened either — nothing
            // else will hover this, so say why it goes nowhere. With candidates, the
            // substitution hover already explains the `{{ }}`.
            Some(SkipReason::Templated) if res.candidates.is_empty() => Some(
                Md::new()
                    .line("Skipped".bold() + " — value only known at runtime; no file matches"),
            ),
            Some(SkipReason::Templated) => None,
            Some(SkipReason::NotInWorkspace) => Some(Md::new().line(
                "Skipped".bold() + " — not in this workspace (a builtin, or installed outside it)",
            )),
            Some(SkipReason::GroupAlternative) => Some(Md::new().line(
                "Absent".bold()
                    + " — a first-match `vars_files` alternative; the list warns only when \
                       none of its files exists",
            )),
            // T-218. This used to fall through to `NotInWorkspace` and tell the reader the
            // role was "installed outside" a workspace it is sitting in. The role directory
            // was found; naming it is the whole correction, so the probed `tasks/main.*`
            // paths are carried through to be shortened back into it here.
            Some(SkipReason::RoleWithoutMainTasks) => {
                let dir = res.candidates.first().and_then(|c| c.parent()?.parent());
                let line = "Skipped".bold()
                    + " — this role has no `tasks/main.yml`, and needs none: the include's \
                       `tasks_from:` names the file that runs";
                Some(match dir {
                    Some(d) => Md::new().line(line).line(md::text("Role: ") + path_span(d, ctx)),
                    None => Md::new().line(line),
                })
            }
            None => None,
        },
    }
}

/// Hover for a variable Ansible injects, for the two whose value the detected install knows.
///
/// Deliberately narrow. Every other magic name and every `ansible_*` fact keeps hovering
/// nothing: a popup saying "provided by Ansible at runtime" on tokens we can say nothing
/// concrete about is the tool talking to hear itself, and widening later is additive.
///
/// Both lines name the install, because both values are the *editor's* Ansible and the play
/// may well run on another one — CI, tox, a second venv (T-051). A hover that states the
/// source can be argued with; one that just asserts a path cannot.
fn injected_var_hover(name: &str, install: Option<&AnsibleInstall>) -> Option<String> {
    let source = |install: &AnsibleInstall| match install.package_dir.as_ref() {
        Some(p) => md::text(&format!("from the detected install at {}", p.display())),
        None => md::text("from the detected install"),
    };
    // A value the resolver actually holds comes first. When it holds none, the name still
    // gets the table's line below — silence here used to read as "not a variable".
    let valued = match (name, install) {
        ("ansible_playbook_python", Some(i)) => i.python.as_ref().map(|py| {
            Md::new()
                .line(md::text("`ansible_playbook_python` — the interpreter Ansible runs on"))
                .line(md::code(&py.display().to_string()))
                .gap()
                .line(source(i).italic())
                .line(
                    md::text(
                        "The running play's own interpreter may differ — this is the one \
                         behind the `ansible` this editor found.",
                    )
                    .italic(),
                )
                .render()
        }),
        // A dict at runtime (`full`, `major`, `minor`, `revision`, `string`), so the hover
        // reports the release rather than implying the bare name is a string.
        ("ansible_version", Some(i)) => i.version.as_ref().map(|v| {
            Md::new()
                .line(md::text("`ansible_version` — ansible-core, as a dict"))
                .line(md::code(&format!("{v}")))
                .gap()
                .line(source(i).italic())
                .render()
        }),
        _ => None,
    };
    if valued.is_some() {
        return valued;
    }
    if let Some(row) = injected::injected(name) {
        let core = install.and_then(|i| i.version);
        // The deprecation is dated by ansible; whether it has already bitten depends on
        // the core this editor found, and the line says which side of it this install is.
        let removal = row.removed_in.map(|gone| match core {
            Some(v) if v >= gone => md::text(&format!(
                "Removed in ansible-core {gone}. The detected install is {v}, so this read is undefined."
            )),
            Some(v) => md::text(&format!("Deprecated: removed in ansible-core {gone}. The detected install is {v}.")),
            None => md::text(&format!("Deprecated: removed in ansible-core {gone}.")),
        });
        let mut doc = Md::new()
            .line(md::code(name) + " — " + md::raw(row.meaning))
            .gap()
            .line(
                (md::text("Set by ansible (")
                    + md::raw(row.set_by)
                    + "); "
                    + md::raw(row.scope.describe())
                    + ".")
                    .italic(),
            );
        if let Some(line) = removal {
            doc = doc.line(line.bold());
        }
        return Some(doc.render());
    }
    if injected::may_be_fact(name) {
        return Some(
            Md::new()
                .line(md::code(name) + " — not set by ansible-core itself")
                .gap()
                .line(
                    md::raw(
                        "An `ansible_`-prefixed name: a fact gathered from the host, or a \
                         connection variable set in inventory. Nothing in this workspace \
                         defines it.",
                    )
                    .italic(),
                )
                .render(),
        );
    }
    None
}

/// A name ansible does provide, read where ansible does not provide it (T-224). The scope
/// it needs is the whole message; "never defined" would send the reader hunting for a
/// definition that no file could hold.
fn scope_gap_message(u: &vars::VarUse, scope: injected::Scope) -> String {
    use injected::Scope::*;
    match scope {
        Loop => match &u.site.loop_var {
            Some(lv) => format!(
                "`{}` is undefined here: `loop_control: loop_var` names this loop's item `{}` \
                 instead.",
                u.name, lv
            ),
            None => format!(
                "`{}` is set only while a `loop:` / `with_*` runs — it is not defined here.",
                u.name
            ),
        },
        ExtendedLoop => format!(
            "`{}` is set only in a loop with `loop_control: extended: true`.",
            u.name
        ),
        IndexVar => format!("`{}` is set only in a loop with `loop_control: index_var:`.", u.name),
        Role | ChildRole => format!(
            "`{}` is set only inside a role — a play's own task is not in one.",
            u.name
        ),
        Delegated => format!("`{}` is set only on a task with `delegate_to:`.", u.name),
        Template => format!(
            "`{}` is set only inside a file the `template` action renders — not in the task.",
            u.name
        ),
        Always => format!("`{}` is not defined here.", u.name),
    }
}

/// Everything the hover request decides, given a parsed document and a byte offset. Kept
/// out of the async handler because the whole point of T-078 is the *precedence* between
/// the three hovers that can claim a token, and precedence is what wants a test.
///
/// One token, one hover, in this order:
///
/// 1. the `when:` **keyword** — the guard explained in English
/// 2. a reference span (an include path, a role name, a module name) — target/provenance
/// 3. anything else, including the `when:` **value** — the variable under the cursor
fn hover_at(
    doc: &Document,
    nodes: &[Node],
    path: &Path,
    byte: usize,
    settings: Settings,
    open: &OpenDocs,
    inv: &[PathBuf],
    cache: &Mutex<VarCache>,
    install: Option<&Arc<AnsibleInstall>>,
) -> Option<(String, Range)> {
    let ctx = FileContext::discover(path).with_install(install.cloned());
    let range = |s: Span| {
        let (sl, sc) = doc.byte_to_lsp(s.start);
        let (el, ec) = doc.byte_to_lsp(s.end);
        Range::new(Position::new(sl, sc), Position::new(el, ec))
    };

    // The reference under the cursor, extracted but not yet resolved. Resolving every
    // reference in the file to answer a hover on one is the cost this path avoids:
    // only the hovered reference is resolved, and only if a branch below needs it.
    let extracted = references::extract(nodes);
    let in_playbook = extracted.in_playbook;
    let mut refs = extracted.refs;
    if path.ends_with("meta/main.yml") && ctx.role_dir.is_some() {
        refs.extend(references::meta_dependencies(nodes));
    }

    // The condition is attached to the module/include reference (other consumers need it
    // there), but anchoring its *explanation* on that reference made one token mean two
    // things: with inlay hints on you could never see a module's provenance, with them off
    // you could never see the condition. The keyword means "this guard" and nothing else,
    // so it owns the explanation — and the condition value is left to the variable hover,
    // so `cmd_result` in `when: cmd_result.rc == 0` still says where it was registered.
    // Not gated on the inlay-hints setting: that setting is about inlay hints. Needs no
    // resolution, so it never touches the disk.
    if let Some(r) = refs.iter().find(|r| {
        !r.conditions.is_empty()
            && r.condition_key_span
                .is_some_and(|s| s.start <= byte && byte <= s.end)
    }) {
        let s = r.condition_key_span.expect("matched on Some above");
        return Some((when_hover(r), range(s)));
    }

    if let Some(r) = refs.iter().find(|r| r.span.start <= byte && byte <= r.span.end) {
        // Resolve just this reference. A templated path gets the substitution hover
        // (what the `{{ }}` expands to and where those values are defined); a literal
        // one gets the resolved target and the candidates tried, winner marked.
        let defs = cached_definitions(path, nodes, open, inv, cache, install);
        let literals = vars::known_literals(&defs, path, &doc.text);
        let res = resolve::Resolver { literals: Some(&literals), in_playbook, ..Default::default() }
            .resolve(r, &ctx);
        if r.templated {
            if let Some(hit) = Backend::path_substitution_hover(doc, nodes, r, &res, path, open, inv, cache, install) {
                return Some(hit);
            }
        }
        // Resolved refs stay quiet unless asked for (the target is a Cmd+click away).
        // Two exceptions show regardless: an ambiguous ref — several targets, where no
        // single one is a click away and the decoration only gives the count — and
        // modules, whose provenance line states facts a click doesn't (clicking opens
        // the file; it doesn't say what its location means). Skip reasons always show:
        // one line answering "why isn't this coloured" — the setting gates the verbose
        // path dump, not explanations. Missing refs never hover: their diagnostic
        // already lists what was tried.
        let wanted = match res.status {
            Status::Skipped => true,
            Status::Resolved => {
                res.targets.len() > 1
                    || r.kind == ReferenceKind::Module
                    || settings.candidates_on_resolved
            }
            Status::Missing => false,
        };
        // A guard is a property of the *edge*, so it belongs on the reference — that much
        // the old code had right. What it got wrong was letting the guard *replace* the
        // reference hover instead of appending to it, which buried module provenance. It
        // also means a guarded reference is worth hovering even when a resolved single
        // target otherwise wouldn't be: "this include may not run" is not a Cmd+click away.
        let body = wanted
            .then(|| reference_hover(r, &res, &ctx, settings.candidates_on_resolved))
            .flatten();
        let guard = (!r.conditions.is_empty()).then(|| guard_line(r));
        let doc = match (body, guard) {
            (Some(b), g) => Some(b.maybe(g)),
            // Nothing else wanted the hover, so the guard needs its own anchor: where the
            // edge goes, then the condition on it. Not the `Tried:` dump — that stays
            // behind `candidatesOnResolved`.
            (None, Some(g)) => Some(target_line(&res, &ctx).unwrap_or_default().concat(g)),
            (None, None) => None,
        };
        if let Some(doc) = doc {
            return Some((doc.render(), range(r.span)));
        }
    }

    // Variable hover: where the variable under the cursor is defined, and its value.
    Backend::variable_hover_at(doc, nodes, byte, path, open, inv, cache, ctx.install.as_ref())
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, p: InitializeParams) -> Result<InitializeResult> {
        if let Ok(mut roots) = self.state.roots.lock() {
            if let Some(folders) = p.workspace_folders {
                roots.extend(folders.iter().filter_map(|f| f.uri.to_file_path().ok()));
            }
            #[allow(deprecated)]
            if roots.is_empty() {
                if let Some(uri) = p.root_uri.and_then(|u| u.to_file_path().ok()) {
                    roots.push(uri);
                }
            }
        }

        // Logged rather than applied silently: "the setting does nothing" is otherwise
        // indistinguishable from "the client never sent it", and in a multi-root window
        // a folder-level settings.json is ignored for window-scoped keys.
        let received = match &p.initialization_options {
            Some(opts) => {
                if let Ok(mut s) = self.state.settings.lock() {
                    *s = Settings::from_json(opts);
                }
                self.state.set_inventory(opts);
                // Which Ansible to index, when several exist or none is on PATH. Recorded
                // here and handed to `AnsibleInstall::init` by `startup`, which is the only
                // place detection runs.
                if let Ok(mut slot) = self.state.ansible_path.lock() {
                    *slot = opts
                        .get("ansiblePath")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.trim().is_empty())
                        .map(PathBuf::from);
                }
                opts.to_string()
            }
            None => "none — client sent no initializationOptions".to_string(),
        };
        self.state.startup_note.lock().map(|mut n| *n = received).ok();

        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "ansible-lsp".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                definition_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                document_link_provider: Some(DocumentLinkOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: Default::default(),
                }),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: SemanticTokensLegend {
                                token_types: SEMANTIC_TOKEN_LEGEND.to_vec(),
                                token_modifiers: SEMANTIC_TOKEN_MODIFIERS.to_vec(),
                            },
                            // Full-document only: a template is a few KB and the whole file
                            // re-tokenises in well under a frame, so `range` and delta
                            // requests would be machinery with nothing to buy.
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: Some(false),
                            work_done_progress_options: Default::default(),
                        },
                    ),
                ),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        let note = self.state.startup_note.lock().map(|n| n.clone()).unwrap_or_default();
        let s = self.state.settings.lock().map(|s| *s).unwrap_or_default();
        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "ansible-lsp ready — initializationOptions: {note} | effective: \
                     inlayHints.enabled={}",
                    s.hints
                ),
            )
            .await;
        // Detached (T-075): the message pump must go back to servicing hover/goto while
        // startup work runs. Everything it touches lives behind `Arc<State>`.
        let task = tokio::spawn(Self::startup(self.state.clone(), self.client.clone()));
        if let Ok(mut slot) = self.state.scan_task.lock() {
            *slot = Some(task);
        }
    }

    async fn did_change_configuration(&self, p: DidChangeConfigurationParams) {
        if let Ok(mut s) = self.state.settings.lock() {
            *s = Settings::from_json(&p.settings);
        }
        self.state.set_inventory(&p.settings);
        // Re-publish: the status bar must follow the change, or picking an inventory
        // silently leaves the old name on screen — the exact ambiguity this shows to fix.
        Self::publish_inventory(&self.state, &self.client).await;
        let s = self.state.settings.lock().map(|s| *s).unwrap_or_default();
        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "settings changed — received: {} | effective: hints={} candidatesOnResolved={}",
                    p.settings, s.hints, s.candidates_on_resolved
                ),
            )
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    /// What a `when:` means, on hover over the conditional reference.
    ///
    /// Hover, not an inlay: the explanation is wanted on demand, not painted onto every
    /// conditional line where it clutters the file and collides with the editor's own
    /// end-of-line blame. Hover has room to spell out every clause instead of a truncated
    /// stub. Plain LSP, so it carries to Neovim, unlike the teal decoration.
    async fn hover(&self, p: HoverParams) -> Result<Option<Hover>> {
        let settings = self.state.settings.lock().map(|s| *s).unwrap_or_default();
        let uri = &p.text_document_position_params.text_document.uri;
        let pos = p.text_document_position_params.position;
        let Some(text) = self.state.text_of(uri) else {
            return Ok(None);
        };
        let Ok(path) = uri.to_file_path() else {
            return Ok(None);
        };
        let doc = Document::new(text);
        let Some(nodes) = doc.parse() else {
            return Ok(None);
        };
        let byte = doc.lsp_to_byte(pos.line, pos.character);
        let inv = self.state.inventory_setting();
        Ok(
            hover_at(
                &doc,
                &nodes,
                &path,
                byte,
                settings,
                &self.state.open_docs(),
                &inv,
                &self.state.var_cache,
                self.state.install().as_ref(),
            )
            .map(
                |(value, range)| Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value,
                    }),
                    range: Some(range),
                },
            ),
        )
    }

    async fn did_open(&self, p: DidOpenTextDocumentParams) {
        let uri = p.text_document.uri;
        if let Ok(path) = uri.to_file_path() {
            invalidate_var_cache(&self.state.var_cache, &path);
            // **Not** `invalidate_render_sites`. Opening a file changes nothing: the buffer
            // that arrives here is what is already on disk, and both caches are derived from
            // disk. Clearing them threw away a walk of every YAML file and every template,
            // which `publish_diagnostics` below then rebuilt inline — once per tab. Measured
            // in the editor: a reloaded window with six templates open showed plain text for
            // 10-15s, and the colours only landed after the last rebuild. An edit still
            // invalidates, in `did_change` and `did_save`, which is where a file's content
            // actually changes.
        }
        if let Ok(mut d) = self.state.docs.lock() {
            d.insert(uri.clone(), p.text_document.text);
        }
        self.publish_diagnostics(&uri).await;
    }

    async fn did_change(&self, mut p: DidChangeTextDocumentParams) {
        // FULL sync: the last change carries the whole document.
        let Some(change) = p.content_changes.pop() else {
            return;
        };
        let uri = p.text_document.uri;
        // This file changed — drop cached variable results that read it (and any that
        // include it), so the next analyse recomputes with the new content.
        if let Ok(path) = uri.to_file_path() {
            invalidate_var_cache(&self.state.var_cache, &path);
            invalidate_render_sites(&self.state, &path);
        }
        if let Ok(mut d) = self.state.docs.lock() {
            d.insert(uri.clone(), change.text);
        }
        self.publish_diagnostics(&uri).await;
    }

    async fn did_close(&self, p: DidCloseTextDocumentParams) {
        if let Ok(path) = p.text_document.uri.to_file_path() {
            // The buffer is gone, so every later answer must come from disk again.
            invalidate_var_cache(&self.state.var_cache, &path);
            invalidate_render_sites(&self.state, &path);
        }
        if let Ok(mut d) = self.state.docs.lock() {
            d.remove(&p.text_document.uri);
        }
        self.client
            .publish_diagnostics(p.text_document.uri, vec![], None)
            .await;
    }

    async fn goto_definition(
        &self,
        p: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = p.text_document_position_params.text_document.uri;
        let pos = p.text_document_position_params.position;

        let Some(text) = self.state.text_of(&uri) else {
            return Ok(None);
        };
        let Ok(path) = uri.to_file_path() else {
            return Ok(None);
        };

        // Same fork as `publish_diagnostics`: a `.j2` is a different grammar, and its
        // references live in `{% include %}` rather than in any YAML key.
        if Self::is_template_file(&path) {
            let root = self.state.roots.lock().ok().and_then(|r| r.first().cloned());
            let hits =
                Self::template_definitions_at(&self.state, &text, pos, &path, root.as_deref());
            return Ok((!hits.is_empty()).then(|| GotoDefinitionResponse::Array(hits)));
        }
        let doc = Document::new(text);
        // Unparseable is expected, not an error: strict YAML 1.2 rejects files the
        // PyYAML Ansible uses accepts. Return nothing rather than guessing.
        let Some(nodes) = doc.parse() else {
            return Ok(None);
        };
        Ok(
            Self::definition_at(
                &doc,
                &nodes,
                pos,
                &uri,
                &path,
                &self.state.open_docs(),
                &self.state.inventory_setting(),
                &self.state.var_cache,
                self.state.install().as_ref(),
            )
                .map(GotoDefinitionResponse::Array),
        )
    }

    /// Jinja syntax colouring, decided by the parser rather than by the client's grammar.
    ///
    /// The client ships a TextMate grammar that paints the same shapes and is wrong where a
    /// regex cannot reach: it hardcodes `{%`, so a template whose `#jinja2:` header renames
    /// the delimiters gets its real tags missed and a literal `{%` painted as a tag. These
    /// tokens are read with the delimiters the file actually renders with — including ones
    /// inherited from the `template:` task, which no standalone grammar of any kind can see.
    ///
    /// Templates only for now. The `{{ … }}` inside a YAML scalar is the larger surface and
    /// is next; it needs the spans `references` already extracts, not new analysis.
    async fn semantic_tokens_full(
        &self,
        p: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = &p.text_document.uri;
        let (Some(text), Ok(path)) = (self.state.text_of(uri), uri.to_file_path()) else {
            return Ok(None);
        };
        // A `.j2` is one template; a YAML file is many small ones, one per scalar. Anything
        // else (a `.cfg`, a `.md`) gets nothing rather than a guess.
        let is_template = Self::is_template_file(&path);
        let is_yaml = matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yml" | "yaml")
        );
        if !is_template && !is_yaml {
            return Ok(None);
        }
        let ctx = FileContext::discover(&path);
        let root = self.state.roots.lock().ok().and_then(|r| r.first().cloned());
        // Answer now, refine later. The grammar map costs a walk of every YAML file and every
        // template, and blocking on it left a reloaded window plain for ~10s — so a cold cache
        // is answered with the default delimiters and a refresh is requested once the real map
        // is built. Being briefly wrong about an overridden delimiter is the same answer the
        // client's grammar gives anyway; being blank for ten seconds is not.
        let cached = Self::template_grammar_if_cached(&self.state, &path, root.as_deref(), &ctx);
        let (d, is_root) = cached.clone().unwrap_or((jinja::Delimiters::default(), true));
        if cached.is_none() {
            let (state, client) = (self.state.clone(), self.client.clone());
            tokio::spawn(async move {
                let ctx = FileContext::discover(&path);
                Self::template_grammar_cached(&state, &path, root.as_deref(), &ctx);
                let _ = client.semantic_tokens_refresh().await;
            });
        }
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: if is_template {
                Self::semantic_tokens_of(&text, &d, is_root)
            } else {
                // No `is_root` for YAML: a scalar cannot carry a `#jinja2:` header, so the
                // grammar that applies is the file's render site's, never its own.
                Self::yaml_semantic_tokens_of(&text, &d)
            },
        })))
    }

    /// Every resolvable reference. The client also paints these, so what's clickable is
    /// visible without hovering.
    async fn document_link(&self, p: DocumentLinkParams) -> Result<Option<Vec<DocumentLink>>> {
        let Some(a) = self.state.analyze(&p.text_document.uri) else {
            return Ok(None);
        };
        Ok(Some(Self::document_links_of(&a, &p.text_document.uri)))
    }
}

/// The token types this server sends, in the order the protocol indexes them: a
/// `SemanticToken.token_type` is a **position in this array**, not a name, so appending is
/// safe and reordering silently recolours everything.
///
/// Standard LSP names only. A client that has never heard of Ansible already has theme rules
/// for these, which is the whole reason to serve tokens rather than paint decorations —
/// see [`crate::LanguageServer::semantic_tokens_full`].
///
/// T-126 wants to colour *resolvable* references from the same provider, and a server has
/// exactly one legend. Its distinction is "does this resolve", which is a property of a
/// token rather than a kind of token, so it belongs in [`SEMANTIC_TOKEN_MODIFIERS`] and
/// needs nothing moved in this list.
const SEMANTIC_TOKEN_LEGEND: &[SemanticTokenType] = &[
    SemanticTokenType::COMMENT,
    SemanticTokenType::KEYWORD,
    SemanticTokenType::VARIABLE,
    SemanticTokenType::FUNCTION,
    SemanticTokenType::STRING,
    SemanticTokenType::NUMBER,
    SemanticTokenType::OPERATOR,
    // Not standard. The client maps it to `punctuation.definition.tag` through
    // `contributes.semanticTokenScopes`, which is the scope its grammar used to paint these
    // with — so the delimiters keep the dimmer grey they had before the tag rules were
    // removed, and now do so on a file whose delimiters are not the default pair.
    SemanticTokenType::new("delimiter"),
    // Also not standard, and the one token here that exists to *undo* something. The client's
    // grammar paints `{# … #}` from a hardcoded `{#`; on a template that renames the comment
    // delimiters that text is literal output, and a grammar scope cannot be cleared by
    // staying silent. Mapped to `meta.template.expression`, which is the one scope the shipped
    // dark, light and high-contrast themes all resolve to `editor.foreground` — so the
    // repaint lands on the default text colour rather than a colour of its own.
    SemanticTokenType::new("text"),
    // Two more, for the same reason as `delimiter`: the protocol has no name for either, and
    // both are mapped by the client to a scope the theme already colours. Splitting them is
    // what lets a tag name, a word operator and a literal differ — which Go and Python both
    // do and we did not. Appended, never inserted: an index is a position in this array.
    SemanticTokenType::new("wordOperator"),
    SemanticTokenType::new("constant"),
    // Standard again, so the client maps nothing: VS Code's default table sends `property`
    // to `variable.other.property`, and whether that differs from `variable` is the theme's
    // call. Dark Modern paints both `#9CDCFE`; the point is that a theme *can* tell them apart.
    SemanticTokenType::PROPERTY,
    // Both standard, both from `{% macro %}` / `{% import %}` declarations.
    SemanticTokenType::PARAMETER,
    SemanticTokenType::NAMESPACE,
    // Not standard: neither LSP nor VS Code's registry has a label type. The client maps it
    // to `entity.name.label`, the scope the bundled C/C#/JS/TS grammars give a goto label,
    // so a `{% block name %}` takes whatever colour a theme gives those.
    SemanticTokenType::new("label"),
];

/// The modifiers a token can carry, each one a bit in `token_modifiers_bitset` at its index
/// here. `declaration`, on the `h` of `{% for h in hosts %}`; `defaultLibrary`, on a name
/// Jinja or ansible provides — `range(`, `loop.index`, `hostvars`. Both standard, so a theme
/// that styles a Python loop target or a builtin styles ours; both lexical facts about the
/// file, so unlike the resolvability modifier above they claim nothing about the workspace.
const SEMANTIC_TOKEN_MODIFIERS: &[SemanticTokenModifier] =
    &[SemanticTokenModifier::DECLARATION, SemanticTokenModifier::DEFAULT_LIBRARY];
const DECLARATION_BIT: u32 = 1 << 0;
const DEFAULT_LIBRARY_BIT: u32 = 1 << 1;

/// This token's index into [`SEMANTIC_TOKEN_LEGEND`].
fn legend_index(ty: ansible_core::jinja::TokenType) -> u32 {
    use ansible_core::jinja::TokenType as T;
    match ty {
        T::Comment => 0,
        T::Keyword => 1,
        T::Variable => 2,
        T::Function => 3,
        T::String => 4,
        T::Number => 5,
        T::Operator => 6,
        T::Delimiter => 7,
        T::Text => 8,
        T::WordOperator => 9,
        T::Constant => 10,
        T::Property => 11,
        T::Parameter => 12,
        T::Namespace => 13,
        T::Label => 14,
    }
}

impl Backend {
    /// Semantic tokens for a Jinja source, encoded the way the protocol wants them.
    ///
    /// Split out from the handler so a test asserts what the editor receives — the same
    /// reason `document_links_of` exists, and the lesson T-171 taught.
    ///
    /// Two encoding rules that are easy to get wrong and silent when you do:
    /// - every field is a **delta** from the previous token, and the column delta resets to
    ///   an absolute column whenever the line advances;
    /// - **a token may not span lines.** A `{# … #}` comment routinely does, so one is split
    ///   into a per-line run rather than emitted whole. An over-long token is not rejected by
    ///   the client, it just paints to the end of the line and drops the rest of the file's
    ///   alignment.
    fn semantic_tokens_of(text: &str, d: &jinja::Delimiters, root: bool) -> Vec<SemanticToken> {
        Self::encode_tokens(text, ansible_core::jinja::semantic_tokens(text, d, root))
    }

    /// The Jinja inside a YAML document's scalars — `name: "{{ app_name }}"` in a playbook.
    ///
    /// The larger surface: most Jinja anyone writes lives in YAML, not in a `.j2`. Same
    /// builder as a template, which is why `jinja::highlight` was made span-based and
    /// delimiter-parameterised rather than tied to a file.
    fn yaml_semantic_tokens_of(text: &str, d: &jinja::Delimiters) -> Vec<SemanticToken> {
        let doc = ansible_core::parse::Document::new(text.to_string());
        let Some(nodes) = doc.parse() else { return Vec::new() };
        let mut toks = Vec::new();
        for n in &nodes {
            Self::yaml_scalar_tokens(text, n, d, false, &mut toks);
        }
        // One document's worth of scalars comes out in tree order, which is source order for
        // every shape we walk — but `when:` is read from a mapping whose key was already
        // visited, so sort rather than trust it. The protocol encodes deltas and underflows
        // on an out-of-order token; `jinja::highlight` carries the same assertion.
        toks.sort_by_key(|t: &ansible_core::jinja::SemToken| t.span.start);
        Self::encode_tokens(text, toks)
    }

    /// Walk one node, painting the Jinja in every scalar whose text maps 1:1 back to source.
    ///
    /// `in_when` marks a bare expression: Ansible wraps a `when:` in `{{ }}` itself, so
    /// `foo is defined` is Jinja with no delimiters and reading it as a template would paint
    /// nothing. Same split `vars::uses` already makes.
    fn yaml_scalar_tokens(
        text: &str,
        node: &ansible_core::parse::Node,
        d: &jinja::Delimiters,
        in_when: bool,
        out: &mut Vec<ansible_core::jinja::SemToken>,
    ) {
        use ansible_core::parse::Node;
        match node {
            Node::Scalar { value, span } => {
                // A block scalar's value is not its source text — `>` and `|` strip the
                // indicator and the indent, and a `\"` escape shortens the value — so
                // `span.start + offset` would paint the wrong columns. Measured: for `|` the
                // value is `"literal {{ e }}\n"` while the slice is `"|\n    literal {{ e }}\n"`.
                // Painting the wrong span is worse than painting nothing, so require the
                // exact identity that makes the offsets valid rather than guessing at it.
                if text.get(span.start..span.end) != Some(value.as_str()) {
                    return;
                }
                let inner = if in_when {
                    ansible_core::jinja::expression_tokens(value)
                } else {
                    // `root: false` — a scalar cannot carry a `#jinja2:` header, so its
                    // delimiters come from the file's render site, never from itself.
                    ansible_core::jinja::semantic_tokens(value, d, false)
                };
                out.extend(inner.into_iter().map(|t| ansible_core::jinja::SemToken {
                    span: ansible_core::parse::Span {
                        start: span.start + t.span.start,
                        end: span.start + t.span.end,
                    },
                    ty: t.ty,
                    declaration: t.declaration,
                    default_library: t.default_library,
                }));
            }
            Node::Sequence { items, .. } => {
                for i in items {
                    Self::yaml_scalar_tokens(text, i, d, in_when, out);
                }
            }
            Node::Mapping { entries, .. } => {
                for (k, v) in entries {
                    // Keys are literal text in every mapping but two (`vars::Keys`), and a
                    // key is not where anyone writes Jinja worth painting — walk values only.
                    let when = k.as_str() == Some("when");
                    Self::yaml_scalar_tokens(text, v, d, when, out);
                }
            }
            _ => {}
        }
    }

    /// Spans to the wire: LSP wants each token as a delta from the previous one.
    fn encode_tokens(
        text: &str,
        toks: Vec<ansible_core::jinja::SemToken>,
    ) -> Vec<SemanticToken> {
        let doc = ansible_core::parse::Document::new(text.to_string());
        let mut out = Vec::new();
        let (mut last_line, mut last_col) = (0u32, 0u32);

        for t in toks {
            let ty = legend_index(t.ty);
            let (l0, c0) = doc.byte_to_lsp(t.span.start);
            let (l1, _) = doc.byte_to_lsp(t.span.end);
            for line in l0..=l1 {
                // The slice of this token that falls on `line`, in UTF-16 columns.
                let start = if line == l0 { c0 } else { 0 };
                let end = if line == l1 {
                    doc.byte_to_lsp(t.span.end).1
                } else {
                    let nl = doc.lsp_to_byte(line + 1, 0);
                    doc.byte_to_lsp(nl.saturating_sub(1)).1
                };
                if end <= start {
                    continue;
                }
                let delta_line = line - last_line;
                let delta_start = if delta_line == 0 { start - last_col } else { start };
                out.push(SemanticToken {
                    delta_line,
                    delta_start,
                    length: end - start,
                    token_type: ty,
                    token_modifiers_bitset: (if t.declaration { DECLARATION_BIT } else { 0 })
                        | (if t.default_library { DEFAULT_LIBRARY_BIT } else { 0 }),
                });
                (last_line, last_col) = (line, start);
            }
        }
        out
    }

    /// The body of [`LanguageServer::document_link`], split out so a test covers what the
    /// editor actually receives rather than one ingredient of it.
    ///
    /// Not a style choice: T-171 shipped a host-key jump wired only into
    /// `goto_definition`, its test called the helper directly, and the missing paint
    /// reached the editor. A test that cannot see the assembly does not cover the
    /// assembly, and this is the second time that gap let something through.
    fn document_links_of(a: &Analysis, uri: &Url) -> Vec<DocumentLink> {
        let mut links: Vec<DocumentLink> = a
            .refs
            .iter()
            .filter(|(r, res)| linkable(r, res))
            .filter_map(|(r, res)| {
                let target = res.targets.first()?;
                let (sl, sc) = a.doc.byte_to_lsp(r.span.start);
                let (el, ec) = a.doc.byte_to_lsp(r.span.end);
                Some(DocumentLink {
                    range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
                    target: Some(Url::from_file_path(target).ok()?),
                    tooltip: Some(target.display().to_string()),
                    data: None,
                })
            })
            .collect();
        // T-171: `hostvars['web01']` resolves to a real file, so it is painted like one.
        // Clickable but invisible is a feature nobody finds, and the demo row inviting a
        // click on both names reads as broken when only one of them is coloured.
        if let Ok(path) = uri.to_file_path() {
            links.extend(Self::host_key_links(&a.doc, &path));
        }
        links
    }
}

/// Whether a reference gets a document link. Exactly one resolved target only — a link's
/// target wins over the definition provider on Cmd+click, so emitting one for a
/// multi-candidate templated path would silently drop the other candidates. No links for
/// modules: their provenance hover already carries labelled links to both files, and VS
/// Code renders a link's tooltip as an extra hover line — the same path twice. No link
/// for a `vars_files` group anchor: it spans the whole nested list and would paint over
/// the winning alternative's own link.
fn linkable(r: &Reference, res: &Resolution) -> bool {
    res.status == Status::Resolved
        && res.targets.len() == 1
        && r.kind != ReferenceKind::Module
        && r.vars_files_group.is_none()
}

/// A definition's own position, as an editor Location. The span is in *its* file, so the
/// line/column come from that file's text: the file being edited from the in-memory
/// (possibly unsaved) buffer, anything else from disk.
fn located_at(
    d: &vars::Located,
    open_path: &Path,
    open_text: &str,
    open: &OpenDocs,
) -> Option<Location> {
    let target = if d.file == open_path {
        Document::new(open_text.to_string())
    } else {
        Document::new(open.read(&d.file)?)
    };
    let (sl, sc) = target.byte_to_lsp(d.span.start);
    let (el, ec) = target.byte_to_lsp(d.span.end);
    Some(Location {
        uri: Url::from_file_path(&d.file).ok()?,
        range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
    })
}

fn location_at(path: &Path) -> Option<Location> {
    Some(Location {
        uri: Url::from_file_path(path).ok()?,
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
    })
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::build(|client| Backend {
        client,
        state: Arc::new(State {
            docs: Mutex::new(HashMap::new()),
            roots: Mutex::new(Vec::new()),
            flagged: Mutex::new(HashSet::new()),
            render_sites: Default::default(),
            template_grammars: Default::default(),
            mutations: Mutex::new(HashMap::new()),
            settings: Mutex::new(Settings::default()),
            inventory: Mutex::new(Vec::new()),
            startup_note: Mutex::new(String::new()),
            scanning: AtomicBool::new(false),
            var_cache: Mutex::new(VarCache::default()),
            scan_task: Mutex::new(None),
            ansible_path: Mutex::new(None),
            install: Mutex::new(None),
        }),
    })
    .custom_method("ansible/references", Backend::resolved_references)
    .finish();
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use super::Settings;

    /// "Nothing is open in an editor" (T-199). Every test that reads its fixture off disk is
    /// making that claim, so it is spelled rather than left as a bare `Default::default()` —
    /// it is what separates them from a test that deliberately supplies a buffer.
    fn no_buffers() -> super::OpenDocs {
        super::OpenDocs::default()
    }

    /// A variable cache with nothing in it — the honest state for a test that owns no server
    /// (T-201). A test that needs two answers to share one cache (because the *point* is what
    /// the cache does between them) binds one of these and passes it to both calls.
    fn no_cache() -> std::sync::Mutex<super::VarCache> {
        std::sync::Mutex::new(super::VarCache::default())
    }

    /// The whole hover path, on a synthetic install — cursor in a document to rendered
    /// markdown, with no `detect()` anywhere. The two halves were each pinned below while the
    /// join between them was broken, so this is the test that actually says the feature works.
    #[test]
    fn hovering_an_injected_name_renders_the_detected_values() {
        use ansible_core::install::{AnsibleInstall, Version};
        use std::path::PathBuf;

        let text = concat!(
            "- hosts: localhost\n  vars: { base_url: x }\n  tasks:\n",
            "    - debug: { msg: \"{{ ansible_playbook_python }} {{ base_url }}\" }\n",
            "    - debug: { msg: \"{{ ansible_version }}\" }\n",
        );
        let doc = ansible_core::parse::Document::new(text.to_string());
        let nodes = doc.parse().expect("fixture parses");
        let install = AnsibleInstall {
            package_dir: Some(PathBuf::from("/venv/lib/python3.13/site-packages/ansible")),
            python: Some(PathBuf::from("/venv/bin/python")),
            version: Some(Version { major: 2, minor: 21, patch: 2 }),
            ..Default::default()
        };
        // The same two steps `variable_hover_at` takes: one scan, then the injected branch.
        let hover = |needle: &str, i: Option<&AnsibleInstall>| {
            let byte = text.find(needle).expect("needle present") + 1;
            let use_ = ansible_core::vars::any_uses(&nodes)
                .into_iter()
                .find(|u| byte >= u.span.start && byte < u.span.end)?;
            if !ansible_core::injected::provided(&use_.name) {
                return None;
            }
            super::Backend::injected_var_hover_at(&doc, &use_, i)
        };

        let (md, range) = hover("ansible_playbook_python", Some(&install)).expect("hovers");
        assert!(md.contains("/venv/bin/python"), "{md}");
        assert!(md.contains("/venv/lib/python3.13/site-packages/ansible"), "{md}");
        // The highlight covers the name itself, not the whole `{{ }}` or the whole scalar.
        assert_eq!(range.start.line, 3);
        assert_eq!(range.end.character - range.start.character, 23);

        let (md, _) = hover("ansible_version", Some(&install)).expect("hovers");
        assert!(md.contains("2.21.2"), "{md}");

        // An ordinary variable is the definition hover's business, not this one's.
        assert!(hover("base_url }}", Some(&install)).is_none());
        // No install detected yet, or one we learned nothing about: the table's line, and no
        // invented value (T-224).
        let (md, _) = hover("ansible_playbook_python", None).expect("generic line");
        assert!(md.contains("interpreter") && !md.contains("/venv"), "{md}");
        let (md, _) = hover("ansible_version", Some(&AnsibleInstall::default())).expect("generic line");
        assert!(md.contains("dict") && !md.contains("2.21"), "{md}");
    }

    /// T-224: a user's own variable whose name happens to start with `ansible_` is an
    /// ordinary variable. Hover shows its definition and go-to-definition reaches it — the
    /// prefix used to pre-empt both. `plain_custom` beside it is the control: both names
    /// must come out the same way.
    #[test]
    fn a_defined_ansible_prefixed_name_hovers_and_jumps_to_its_definition() {
        let d = std::env::temp_dir().join("ansible-lsp-t224-defined-prefix");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("play.yml");
        let text = concat!(
            "- hosts: all\n  vars:\n    ansible_custom: 1\n    plain_custom: 2\n  tasks:\n",
            "    - debug: { msg: \"{{ ansible_custom }} {{ plain_custom }} {{ ansible_play_name }}\" }\n",
        );
        std::fs::write(&path, text).unwrap();
        let doc = ansible_core::parse::Document::new(text.to_string());
        let nodes = doc.parse().unwrap();
        let uri = tower_lsp::lsp_types::Url::from_file_path(&path).unwrap();

        let hover = |needle: &str| {
            let byte = text.find(needle).unwrap() + 3;
            super::Backend::variable_hover_at(&doc, &nodes, byte, &path, &no_buffers(), &[], &no_cache(), None)
                .map(|h| h.0)
        };
        let jump = |needle: &str| {
            let byte = text.find(needle).unwrap() + 3;
            let (l, c) = doc.byte_to_lsp(byte);
            super::Backend::variable_defs_at(&doc, &nodes, tower_lsp::lsp_types::Position::new(l, c), &uri, &no_buffers(), &[], &no_cache(), None)
        };

        for name in ["ansible_custom", "plain_custom"] {
            let h = hover(&format!("{{{{ {name}")).unwrap_or_else(|| panic!("{name} hovers"));
            assert!(h.contains("play var") && h.contains("play.yml:"), "{name}: {h}");
            assert!(!h.contains("Set by ansible"), "{name} is the user's, not ansible's: {h}");
            let locs = jump(&format!("{{{{ {name}")).unwrap_or_else(|| panic!("{name} jumps"));
            assert_eq!(locs.len(), 1);
            assert_eq!(locs[0].range.start.line, if name == "ansible_custom" { 2 } else { 3 }, "{name}");
        }
        // A name nothing defines still falls through to the table.
        let h = hover("{{ ansible_play_name").expect("table line");
        assert!(h.contains("Set by ansible") && h.contains("`name:`"), "{h}");
        assert!(jump("{{ ansible_play_name").is_none());
    }

    /// T-224: a provided name read outside its scope is reported with the scope it needs,
    /// never as "never defined" — no file could define `item`, and saying so sends the
    /// reader to add one. The unlooped `nope_missing` beside them is the control that the
    /// ordinary message still exists.
    #[test]
    fn a_scope_gap_names_the_scope_it_needs() {
        use tower_lsp::lsp_types::NumberOrString;
        let d = std::env::temp_dir().join("ansible-lsp-t224-scope-gap");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("play.yml");
        let text = concat!(
            "- hosts: all\n  tasks:\n",
            "    - debug: { msg: \"{{ item }} {{ role_name }} {{ nope_missing }}\" }\n",
            "    - debug: { msg: \"{{ item }} {{ ansible_loop }}\" }\n      loop: [1]\n",
            "      loop_control: { loop_var: row }\n",
        );
        std::fs::write(&path, text).unwrap();
        let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
        let msgs: Vec<String> =
            super::Backend::variable_coverage_diagnostics(&a, &path, &a.nodes, &[], &no_cache())
                .into_iter()
                .filter(|d| matches!(&d.code, Some(NumberOrString::String(s)) if s == "var-undefined"))
                .map(|d| d.message)
                .collect();
        assert_eq!(msgs.len(), 5, "{msgs:?}");
        let has = |needle: &str| msgs.iter().any(|m| m.contains(needle));
        assert!(has("`item` is set only while a `loop:`"), "{msgs:?}");
        assert!(has("`role_name` is set only inside a role"), "{msgs:?}");
        assert!(has("`nope_missing` is never defined"), "{msgs:?}");
        assert!(has("`item` is undefined here: `loop_control: loop_var` names this loop's item `row`"), "{msgs:?}");
        assert!(has("`ansible_loop` is set only in a loop with `loop_control: extended: true`"), "{msgs:?}");
        assert!(!msgs.iter().any(|m| m.contains("`item` is never defined")), "{msgs:?}");
    }

    /// T-224: an unknown `ansible_*` name in a facts-free play says why it is reported and
    /// what it might still be, rather than "never defined in any file" — which is true and
    /// beside the point.
    #[test]
    fn an_unknown_ansible_name_in_a_facts_free_play_says_so() {
        use tower_lsp::lsp_types::NumberOrString;
        let d = std::env::temp_dir().join("ansible-lsp-t224-no-facts");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("play.yml");
        let text = "- hosts: all\n  gather_facts: false\n  tasks:\n    - debug: { msg: \"{{ ansible_hostnme }} {{ nope_missing }}\" }\n";
        std::fs::write(&path, text).unwrap();
        let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
        let msgs: Vec<String> =
            super::Backend::variable_coverage_diagnostics(&a, &path, &a.nodes, &[], &no_cache())
                .into_iter()
                .filter(|d| matches!(&d.code, Some(NumberOrString::String(s)) if s == "var-undefined"))
                .map(|d| d.message)
                .collect();
        assert_eq!(msgs.len(), 2, "{msgs:?}");
        assert!(msgs.iter().any(|m| m.starts_with("`ansible_hostnme` is not a name ansible sets, and this play gathers no facts")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.starts_with("`nope_missing` is never defined")), "{msgs:?}");
    }

    /// The gap that made the first cut of T-143 dead code: the renderer was right and nothing
    /// reached it, because `vars::uses` drops injected names before hover ever sees a token.
    /// This pins the token lookup itself — that the name under the cursor is found, with the
    /// span the hover will highlight, and that an ordinary variable is NOT claimed here.
    #[test]
    fn an_injected_name_is_found_under_the_cursor() {
        let text = concat!(
            "- hosts: localhost\n  vars: { base_url: x }\n  tasks:\n",
            "    - debug: { msg: \"{{ ansible_playbook_python }} {{ base_url }}\" }\n",
            "    - debug: { msg: \"ok\" }\n      when: ansible_version is version('2.19', '>=')\n",
        );
        let doc = ansible_core::parse::Document::new(text.to_string());
        let nodes = doc.parse().expect("fixture parses");

        let at = |needle: &str| {
            let byte = text.find(needle).expect("needle present") + 1;
            ansible_core::vars::any_uses(&nodes)
                .into_iter()
                .find(|u| byte >= u.span.start && byte < u.span.end)
                .filter(|u| ansible_core::injected::provided(&u.name))
                .map(|u| u.name)
        };
        assert_eq!(at("ansible_playbook_python").as_deref(), Some("ansible_playbook_python"));
        // Also inside a `when:`, which is scanned as a bare expression, not a `{{ }}` island.
        assert_eq!(at("ansible_version is").as_deref(), Some("ansible_version"));
        // An ordinary variable stays with the definition hover; this view must not claim it.
        assert_eq!(at("base_url }}").as_deref(), None);

        // And the complementary guarantee: the rule-facing view still drops them, so nothing
        // here re-opens the false-"undefined" class that the exemption exists to prevent.
        let rule_view: Vec<String> =
            ansible_core::vars::uses(&nodes).into_iter().map(|u| u.name).collect();
        assert_eq!(rule_view, vec!["base_url".to_string()]);
    }

    /// A synthetic install, so this runs on a machine with no Ansible — which is most of them,
    /// and the reason `finds_the_local_ansible_install` can only early-return.
    #[test]
    fn injected_var_hover_speaks_only_where_it_holds_a_value() {
        use ansible_core::install::{AnsibleInstall, Version};
        use std::path::PathBuf;

        let install = AnsibleInstall {
            package_dir: Some(PathBuf::from("/venv/lib/python3.13/site-packages/ansible")),
            python: Some(PathBuf::from("/venv/bin/python")),
            version: Some(Version { major: 2, minor: 21, patch: 2 }),
            ..Default::default()
        };
        let hover = |n: &str| super::injected_var_hover(n, Some(&install));

        let py = hover("ansible_playbook_python").expect("interpreter is known");
        assert!(py.contains("/venv/bin/python"), "{py}");
        assert!(py.contains("/venv/lib/python3.13/site-packages/ansible"), "names its source");
        assert!(py.contains("may differ"), "concedes the play may run elsewhere");

        let v = hover("ansible_version").expect("version is known");
        assert!(v.contains("2.21.2"), "{v}");
        assert!(v.contains("dict"), "does not imply the bare name is a string");

        // A value-less table name gets its meaning and scope, and no value (T-224).
        let h = hover("inventory_hostname").expect("table line");
        assert!(h.contains("inventory spells it") && h.contains("every task"), "{h}");
        let h = hover("ansible_loop").expect("table line");
        assert!(h.contains("extended: true"), "scope named: {h}");
        // A possible fact is a maybe, and says so.
        let h = hover("ansible_os_family").expect("fact line");
        assert!(h.contains("fact") && h.contains("Nothing in this workspace"), "{h}");
        // Not ansible's at all: nothing to say.
        assert!(hover("base_url").is_none());

        // A deprecated name says so, and which side of the removal this install is on.
        let h = super::injected_var_hover("play_hosts", Some(&install)).expect("table line");
        assert!(h.contains("Deprecated: removed in ansible-core 2.23.0. The detected install is 2.21.2."), "{h}");
        let newer = AnsibleInstall { version: Some(Version { major: 2, minor: 23, patch: 0 }), ..Default::default() };
        let h = super::injected_var_hover("play_hosts", Some(&newer)).expect("table line");
        assert!(h.contains("Removed in ansible-core 2.23.0. The detected install is 2.23.0, so this read is undefined."), "{h}");
        let h = super::injected_var_hover("play_hosts", None).expect("table line");
        assert!(h.contains("Deprecated: removed in ansible-core 2.23.0.") && !h.contains("detected"), "{h}");

        // A known name whose value was not detected invents nothing — the table line, and
        // neither a path nor a version in it.
        let empty = AnsibleInstall::default();
        let h = super::injected_var_hover("ansible_playbook_python", Some(&empty)).expect("table line");
        assert!(!h.contains('/'), "{h}");
        let h = super::injected_var_hover("ansible_version", Some(&empty)).expect("table line");
        assert!(!h.contains("2.21"), "{h}");
    }

    /// T-102 end to end, against the two checked-in fixtures. The YAML file must report
    /// exactly what real ansible reported (four keys, four lines, live-verified on 2.21.2);
    /// the JSON file must report the same way *and* say Ansible doesn't. Severity follows
    /// `duplicate_dict_key` for both, and `ignore` silences both.
    #[test]
    fn duplicate_keys_report_identically_in_json_but_say_ansible_does_not() {
        use ansible_core::config::DuplicateDictKey;
        use tower_lsp::lsp_types::DiagnosticSeverity;

        let load = |rel: &str| {
            let path = std::path::Path::new(rel).canonicalize().unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            super::Backend::analyze_text(text, &path).unwrap()
        };
        let mut yaml = load("../../demo/duplicate_keys.yml");
        let mut json = load("../../demo/plays/duplicate_keys_json.yml");
        assert_eq!(yaml.doc.loader(), ansible_core::parse::Loader::Yaml);
        assert_eq!(json.doc.loader(), ansible_core::parse::Loader::Json);

        // The fixture's own comments claim these four; ansible printed exactly them.
        let d = super::Backend::duplicate_key_diagnostics(&yaml);
        let lines: Vec<u32> = d.iter().map(|d| d.range.start.line + 1).collect();
        assert_eq!(lines, [23, 29, 37, 42], "must match the live ansible run");
        assert!(d.iter().all(|d| d.severity == Some(DiagnosticSeverity::WARNING)));
        assert!(
            d.iter().all(|d| !d.message.contains("parse as JSON")),
            "the YAML file must not carry the JSON note"
        );
        assert!(d[1].message.contains("`http_port`") && d[1].message.contains("line 28"));

        // Same rule, same severity, one extra sentence.
        let j = super::Backend::duplicate_key_diagnostics(&json);
        assert_eq!(j.len(), 1);
        assert_eq!(j[0].severity, Some(DiagnosticSeverity::WARNING), "not a lesser tier");
        assert!(j[0].message.contains("Ansible does not report this one"), "{}", j[0].message);

        // `error` promotes both; `ignore` silences both.
        for cfg in [DuplicateDictKey::Error, DuplicateDictKey::Ignore] {
            yaml.ctx.config.duplicate_dict_key = cfg;
            json.ctx.config.duplicate_dict_key = cfg;
            let (y, j) = (
                super::Backend::duplicate_key_diagnostics(&yaml),
                super::Backend::duplicate_key_diagnostics(&json),
            );
            match cfg {
                DuplicateDictKey::Error => {
                    assert!(
                        y.iter().chain(&j).all(|d| d.severity == Some(DiagnosticSeverity::ERROR)),
                        "error must reach the JSON file too"
                    );
                    assert_eq!((y.len(), j.len()), (4, 1));
                }
                _ => assert!(y.is_empty() && j.is_empty(), "ignore means silence everywhere"),
            }
        }
    }

    /// T-062 box 6: a `.yml` inventory that fails YAML parsing is an ERROR naming the
    /// silent INI fallback — "a play that loads this file will fail" is the one claim
    /// that is wrong there. Measured on 2.21.2: a group whose colons were forgotten came
    /// back as a *host*, exit 0, empty stderr.
    #[test]
    fn a_broken_inventory_yml_names_the_ini_fallback_instead_of_the_generic_error() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let broken = "all:\n\thosts:\n".to_string();

        let inv = super::State::unparseable_diagnostic_for(broken.clone(), true);
        assert_eq!(inv.len(), 1);
        assert_eq!(inv[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(inv[0].code, Some(NumberOrString::String("inventory-not-yaml".into())));
        assert!(inv[0].message.contains("INI"), "{}", inv[0].message);

        // The control: the same text as an ordinary file keeps the generic claim, which
        // is what stops this test passing on a message that never varies.
        let plain = super::State::unparseable_diagnostic_for(broken, false);
        assert_eq!(plain[0].code, Some(NumberOrString::String("unparseable".into())));
        assert!(!plain[0].message.contains("INI"), "{}", plain[0].message);
    }

    /// The detection half of the rule above: the open file is an inventory when it is one
    /// of the sources the file's own `ansible.cfg` resolves, and its neighbour is not.
    #[test]
    fn a_configured_inventory_file_is_recognised_and_its_neighbour_is_not() {
        let d = std::env::temp_dir().join("ansible-lsp-inv-not-yaml");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("ansible.cfg"), "[defaults]\ninventory = inv.yml\n").unwrap();
        std::fs::write(d.join("inv.yml"), "all:\n\thosts:\n").unwrap();
        let cache = super::ScanCache::default().with_env(ansible_core::config::EnvMap::empty());
        assert!(super::State::is_inventory_source(&d.join("inv.yml"), &cache));
        assert!(!super::State::is_inventory_source(&d.join("play.yml"), &cache));
    }

    /// T-016 against the real demo: exactly the labeled BAD cases warn — the missing
    /// single, the no-extension miss, and the all-missing group anchored on the whole
    /// nested list — while every GOOD/NO-HINT case stays silent.
    #[test]
    fn vars_files_demo_diagnoses_singles_and_groups() {
        use tower_lsp::lsp_types::{Diagnostic, NumberOrString};
        let path = std::path::Path::new("../../demo/vars_files_demo.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let diags = super::Backend::diagnostics_of(&a);
        let is_missing = |d: &&Diagnostic| {
            matches!(&d.code, Some(NumberOrString::String(s)) if s == "missing-file")
        };
        let missing: Vec<_> = diags.iter().filter(is_missing).collect();
        assert_eq!(missing.len(), 3, "exactly the labeled BAD cases:\n{missing:#?}");
        assert!(missing.iter().any(|d| d.message.contains("vars/not_exists.yml")));
        assert!(
            missing.iter().any(|d| d.message.contains("`shared`")),
            "the no-extension-guessing case warns"
        );
        let group = missing
            .iter()
            .find(|d| d.message.contains("vars/nope-a.yml"))
            .expect("group diagnostic");
        for alt in ["vars/nope-a.yml", "vars/nope-b.yml", "vars/nope-c.yml"] {
            assert!(group.message.contains(alt), "group names all three:\n{}", group.message);
        }
        assert!(
            group.range.end.line > group.range.start.line,
            "anchored on the whole nested list, not one entry"
        );
        assert!(
            !missing.iter().any(|d| d.message.contains("site-local")),
            "a satisfied group stays silent"
        );
    }

    /// T-087, rule 4: the demo's fatal rows are labelled with the type ansible-core dies
    /// with, and a label is a claim. Every one of them is pinned here, including that they
    /// are ERRORs — the whole point of the section is that these do not merely warn.
    #[test]
    fn vars_files_demo_flags_every_shape_that_cannot_name_a_file() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path = std::path::Path::new("../../demo/vars_files_demo.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text.clone(), &path).unwrap();
        let bad: Vec<_> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(|d| {
                matches!(&d.code, Some(NumberOrString::String(s)) if s == "invalid-vars-files-entry")
            })
            .collect();
        assert_eq!(bad.len(), 4, "exactly the four labelled rows: {bad:#?}");
        for d in &bad {
            assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR), "{}", d.message);
            assert!(d.message.contains("play fails to start"), "{}", d.message);
        }
        let types = |t: &str| bad.iter().filter(|d| d.message.contains(t)).count();
        // Two NoneType rows: the bare `-`, and the null alternative inside a group.
        assert_eq!(types("'NoneType'"), 2, "{bad:#?}");
        assert_eq!(types("'dict'"), 1, "{bad:#?}");
        assert_eq!(types("'list'"), 1, "{bad:#?}");

        // A null item has no text of its own, so its range must still cover something —
        // a zero-width squiggle is one nobody sees.
        let lines: Vec<&str> = text.lines().collect();
        for d in &bad {
            assert!(
                d.range.end > d.range.start,
                "empty range on {:?}",
                lines.get(d.range.start.line as usize)
            );
        }
    }

    /// T-087: the directory row is its own rule and its own severity, and its message says
    /// the opposite of the missing-file one four plays above it. Both claims live in this
    /// same file, so this is where they must not contradict each other.
    #[test]
    fn vars_files_demo_directory_entry_is_fatal_not_a_miss() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path = std::path::Path::new("../../demo/vars_files_demo.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let dir: Vec<_> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(|d| {
                matches!(&d.code, Some(NumberOrString::String(s)) if s == "vars-files-directory")
            })
            .collect();
        assert_eq!(dir.len(), 1, "one labelled directory row: {dir:#?}");
        assert_eq!(dir[0].severity, Some(DiagnosticSeverity::ERROR));
        assert!(dir[0].message.contains("Errno 21"), "{}", dir[0].message);
        assert!(dir[0].message.contains("does not start"), "{}", dir[0].message);
        assert!(
            !dir[0].message.contains("silently skips"),
            "the miss wording must not appear on the fatal case: {}",
            dir[0].message
        );
    }

    /// The false-positive gate for both rules: `vars_files` is written all over the demo
    /// tree and every other use of it is legal.
    #[test]
    fn every_other_demo_file_is_free_of_vars_files_entry_diagnostics() {
        use tower_lsp::lsp_types::NumberOrString;
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let files = ansible_core::workspace::yaml_files(&demo);
        assert!(files.len() > 10, "the demo walk found the demo");
        for path in files {
            if path.file_name().is_some_and(|n| n == "vars_files_demo.yml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Some(a) = super::Backend::analyze_text(text, &path) else { continue };
            let bad: Vec<String> = super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter(|d| {
                    matches!(&d.code, Some(NumberOrString::String(s))
                        if s == "invalid-vars-files-entry" || s == "vars-files-directory")
                })
                .map(|d| d.message)
                .collect();
            assert!(bad.is_empty(), "{}: {bad:?}", path.display());
        }
    }

    /// T-010: each rule answers to its own id and not to the other's — they describe
    /// opposite runtime behaviour, so one `# noqa` must not silence both.
    #[test]
    fn noqa_suppresses_each_vars_files_rule_by_its_own_id() {
        use tower_lsp::lsp_types::NumberOrString;
        let path = std::path::Path::new("../../demo").canonicalize().unwrap().join("probe.yml");
        let ids = |text: &str| -> Vec<String> {
            let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
            super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter_map(|d| match &d.code {
                    Some(NumberOrString::String(s))
                        if s == "invalid-vars-files-entry" || s == "vars-files-directory" =>
                    {
                        Some(s.clone())
                    }
                    _ => None,
                })
                .collect()
        };
        let shape = "- hosts: all
  vars_files:
    - dir: vars
";
        assert_eq!(ids(shape), ["invalid-vars-files-entry"]);
        assert!(ids("- hosts: all
  vars_files:
    - dir: vars  # noqa: invalid-vars-files-entry
")
            .is_empty());
        // The other rule's id does not reach it.
        assert_eq!(
            ids("- hosts: all
  vars_files:
    - dir: vars  # noqa: vars-files-directory
"),
            ["invalid-vars-files-entry"]
        );

        let dirs = "- hosts: all
  vars_files:
    - vars
";
        assert_eq!(ids(dirs), ["vars-files-directory"]);
        assert!(ids("- hosts: all
  vars_files:
    - vars  # noqa: vars-files-directory
")
            .is_empty());
    }

    /// T-117. The detected core version decides how loudly a strictness fault is reported,
    /// and the demo fixture carries both shapes. Measured upstream: an empty condition runs
    /// silently on 2.18.6 and is fatal on 2.21.2, so the old core gets a warning that it
    /// breaks on upgrade rather than the silence Ansible itself gives it.
    #[test]
    fn strictness_diagnostics_take_their_severity_from_the_core_version() {
        use ansible_core::install::Version;
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path = std::path::Path::new("../../demo/tasks/conditions.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();

        let strict = |core| {
            super::Backend::diagnostics_with(&a, core)
                .into_iter()
                .filter(|d| {
                    matches!(&d.code, Some(NumberOrString::String(s))
                        if s == "when-empty" || s == "when-not-boolean")
                })
                .collect::<Vec<_>>()
        };

        let new = strict(Some(Version { major: 2, minor: 21, patch: 2 }));
        assert!(!new.is_empty(), "the demo must keep demonstrating these");
        assert!(
            new.iter().all(|d| d.severity == Some(DiagnosticSeverity::ERROR)),
            "fatal on 2.21.2, so an error: {new:#?}"
        );
        assert!(
            new.iter().any(|d| d.message.contains("2.21.2")),
            "the message names the runtime it describes"
        );

        // Same faults, same count, quieter — never silent, or the upgrade break goes unsaid.
        let old = strict(Some(Version { major: 2, minor: 18, patch: 6 }));
        assert_eq!(old.len(), new.len(), "an old core hides nothing");
        assert!(old.iter().all(|d| d.severity == Some(DiagnosticSeverity::WARNING)));
        assert!(old.iter().any(|d| d.message.contains("2.19")), "names the upgrade");

        // No install detected is the common case and must behave like the old core.
        assert!(strict(None).iter().all(|d| d.severity == Some(DiagnosticSeverity::WARNING)));

        // A null `when:` is absence, not an empty condition — the parser has to keep them
        // apart or this fires on the demo's GOOD case.
        assert_eq!(
            new.iter().filter(|d| matches!(&d.code, Some(NumberOrString::String(s)) if s == "when-empty")).count(),
            3,
            "exactly the three empty-string cases, not the null one"
        );
    }

    /// T-107. Each rejected key is anchored on itself, carries the class Ansible would
    /// name, and the legal uses around it stay silent. The demo config keeps
    /// `invalid_task_attribute_failed` at the default, so everything here is an ERROR.
    #[test]
    fn invalid_attribute_diagnostics_name_the_rejecting_class() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path = std::path::Path::new("../../demo/invalid_attributes.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let got: Vec<_> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(|d| matches!(&d.code, Some(NumberOrString::String(s)) if s == "invalid-attribute"))
            .collect();

        let messages: Vec<&str> = got.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "'when' is not a valid attribute for a Play",
                "'loop' is not a valid attribute for a Block",
                "'listen' is not a valid attribute for a Task",
                "'become' is not a valid attribute for a TaskInclude",
                "Invalid options for import_tasks: apply",
                "'retries' is not a valid attribute for a Block",
                "'name' is not a valid attribute for a LoopControl",
            ],
            "one diagnostic per BAD line, none for the GOOD ones"
        );
        assert!(got.iter().all(|d| d.severity == Some(DiagnosticSeverity::ERROR)));
        // Anchored on the key itself: the play's `when`, not the whole play.
        assert_eq!(got[0].range.start.line, got[0].range.end.line);
    }

    /// T-063: the BAD rows of the arg-surface demo, and only those. The GOOD rows are the
    /// other half — eleven legal args, most of which do nothing statically, all of which a
    /// stricter arg list would turn into false errors on a working play.
    #[test]
    fn the_role_include_params_demo_reports_exactly_its_bad_rows() {
        use tower_lsp::lsp_types::NumberOrString;
        let path = std::path::Path::new("../../demo")
            .canonicalize()
            .unwrap()
            .join("tasks/role_include_params.yml");
        let text = std::fs::read_to_string(&path).expect("demo fixture");
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let msgs: Vec<String> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(|d| {
                matches!(&d.code, Some(NumberOrString::String(s)) if s == "invalid-attribute")
            })
            .map(|d| d.message)
            .collect();
        assert_eq!(
            msgs,
            [
                "'retries' is not a valid attribute for a Block",
                "Invalid options for ansible.builtin.import_role: apply",
                "Invalid options for ansible.builtin.import_role: rescuable",
                "Invalid options for ansible.builtin.include_role: frobnicate",
            ]
        );
    }

    /// T-147's corpus gate, kept runnable rather than done once and described in a ticket.
    ///
    /// `ANSIBLE_CORPUS=<dir> cargo test -p ansible-lsp role_metadata_corpus -- --ignored --nocapture`
    ///
    /// Point it at one tree at a time. Every hit is printed with its file and line, because
    /// the gate is "read each one": this rule calls valid Ansible fatal if it is wrong, so a
    /// bare count cannot tell a real find from a rule that has started guessing.
    ///
    /// | tree | commit | meta files seen | hits |
    /// | ------------------------------------------ | --------- | -------------- | ---- |
    /// | `kubernetes-sigs/kubespray`                | `46dbdd3` | 62 (31 distinct) | 0 |
    /// | `debops/debops`                            | `65b66ff` | 812 (203 distinct) | 0 |
    /// | `geerlingguy/ansible-role-mysql`           | `0a0ea6b` | 1              | 0 |
    /// | `ansible/ansible-examples`                 | `b505865` | 0              | — |
    ///
    /// The counts are files *walked*, not distinct files: [`yaml_files`] follows directory
    /// symlinks, and both trees point a second path at their whole role tree
    /// (`extra_playbooks/roles -> ../roles`, `debops/roles -> ansible/roles`), so each role
    /// is visited more than once. Left as-is — following them is right for resolution, since
    /// Ansible would load through those paths too — but it means a hit count from this gate
    /// would double-report, and a distinct-file count needs `find`, not this walk.
    ///
    /// `ansible-examples` has no role `meta/` at all, so it is listed as measured and
    /// proving nothing rather than as a tree that passed.
    ///
    /// **Zeros are also what a broken sweep looks like**, so the control that must come out
    /// different is this repo's own `demo/`, which reports exactly 3 — the BAD rows of
    /// `roles/metadata-keys/meta/main.yml`. If that comes back 0, the sweep is broken.
    #[test]
    #[ignore = "corpus gate: ANSIBLE_CORPUS=<path> cargo test -p ansible-lsp role_metadata_corpus -- --ignored --nocapture"]
    fn role_metadata_corpus() {
        use tower_lsp::lsp_types::NumberOrString;
        let Ok(root) = std::env::var("ANSIBLE_CORPUS") else { return };
        let root = std::path::PathBuf::from(root);
        if !root.is_dir() {
            return;
        }
        let (mut hits, mut metas) = (Vec::new(), 0);
        for f in ansible_core::workspace::yaml_files(&root) {
            let Ok(t) = std::fs::read_to_string(&f) else { continue };
            let Some(a) = super::Backend::analyze_text(t, &f) else { continue };
            if !a.is_role_metadata {
                continue;
            }
            metas += 1;
            for d in super::Backend::diagnostics_of(&a) {
                if matches!(&d.code, Some(NumberOrString::String(c)) if c == "invalid-attribute") {
                    hits.push(format!(
                        "{}:{} {}",
                        f.strip_prefix(&root).unwrap_or(&f).display(),
                        d.range.start.line + 1,
                        d.message
                    ));
                }
            }
        }
        println!("invalid-attribute in role metadata: {} hit(s) across {metas} meta file(s)", hits.len());
        for h in &hits {
            println!("  HIT {h}");
        }
    }

    /// T-147: the demo's meta file makes the claim, so the claim is pinned. Three BAD rows,
    /// one SILENCED row that must not appear, and — the half that can fail for the right
    /// reason — `galaxy_info`, `dependencies` and `become:` staying silent above them.
    #[test]
    fn the_role_metadata_demo_reports_exactly_its_bad_rows() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path = std::path::Path::new("../../demo")
            .canonicalize()
            .unwrap()
            .join("roles/metadata-keys/meta/main.yml");
        let text = std::fs::read_to_string(&path).expect("demo fixture");
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let got: Vec<_> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(|d| {
                matches!(&d.code, Some(NumberOrString::String(s)) if s == "invalid-attribute")
            })
            .collect();
        let msgs: Vec<&str> = got.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            msgs,
            [
                "'when' is not a valid attribute for a RoleMetadata",
                "'tags' is not a valid attribute for a RoleMetadata",
                "'author' is not a valid attribute for a RoleMetadata",
            ],
            "one per BAD row; `standalone:` carries a # noqa and the GOOD rows are legal"
        );
        assert!(got.iter().all(|d| d.severity == Some(DiagnosticSeverity::ERROR)));
    }

    /// T-088: no false positives — every demo file except the one built to demonstrate
    /// the rule stays free of invalid-attribute diagnostics.
    #[test]
    fn every_other_demo_file_is_free_of_invalid_attribute_diagnostics() {
        use tower_lsp::lsp_types::NumberOrString;
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let files = ansible_core::workspace::yaml_files(&demo);
        assert!(files.len() > 10, "the demo walk found the demo");
        for path in files {
            // `invalid_attributes.yml` is the rule's own demo. `placement.yml` carries one
            // deliberate case — `loop_control:` on a Block — to show the boundary between
            // this rule and T-155's; its exact expected set is asserted by
            // `the_dead_loop_control_warning_is_its_own_rule`, so it is covered, not exempt.
            // `role_include_params.yml` is the same arrangement for the closed arg set:
            // `the_role_include_params_demo_reports_exactly_its_bad_rows` pins its four.
            // `roles/metadata-keys/meta/main.yml` is T-147's demo, pinned by
            // `the_role_metadata_demo_reports_exactly_its_bad_rows`. Matched as a path
            // suffix, not a file name: every role has a `main.yml`, and exempting that name
            // would excuse the whole demo tree from the rule.
            const DEMOS: [&str; 4] = [
                "invalid_attributes.yml",
                "placement.yml",
                "role_include_params.yml",
                "roles/metadata-keys/meta/main.yml",
            ];
            if DEMOS.iter().any(|d| path.ends_with(d)) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Some(a) = super::Backend::analyze_text(text, &path) else { continue };
            let bad: Vec<String> = super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter(|d| {
                    matches!(&d.code, Some(NumberOrString::String(s)) if s == "invalid-attribute")
                })
                .map(|d| d.message)
                .collect();
            assert!(bad.is_empty(), "{}: {bad:?}", path.display());
        }
    }

    /// T-110. Every message is ansible-core 2.21.2's own, measured by running each case through
    /// `--syntax-check` — Ansible stops at the first fault, we report every one.
    #[test]
    fn placement_diagnostics_match_ansibles_messages() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path = std::path::Path::new("../../demo/placement.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let got: Vec<_> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(|d| matches!(&d.code, Some(NumberOrString::String(s)) if s == "invalid-placement"))
            .collect();

        let messages: Vec<&str> = got.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "both 'user' and 'remote_user' are set for this play. The use of 'user' is \
                 deprecated, and should be removed",
                "Hosts list cannot be empty. Please check your playbook",
                "Hosts list cannot contain values of 'None'. Please check your playbook",
                "Hosts list contains an invalid host value: '{ name: web }'",
                "Hosts list must be a sequence or string. Please check your playbook.",
                "Invalid vars_prompt data structure, found unsupported key 'promt'",
                "Invalid vars_prompt data structure, missing 'name' key",
                "Invalid vars_prompt data structure, missing 'name' key",
                "tags must be specified as a list",
                "tags must be specified as a list",
                "Invalid variable file contents.",
                "Invalid variable file contents.",
                "You cannot use loops on 'import_tasks' statements. You should use \
                 'include_tasks' instead.",
                "You cannot use loops on 'import_tasks' statements. You should use \
                 'include_tasks' instead.",
                "You cannot use loops on 'import_role' statements. You should use \
                 'include_role' instead.",
                "You cannot use loops on 'import_tasks' statements. You should use \
                 'include_tasks' instead.",
                "duplicate loop in task: items",
                "duplicate loop in task: list",
                "you must specify a value when using with_items",
                "you must specify a value when using with_dict",
                "the `loop_control` value must be specified as a dictionary and cannot be a \
                 variable itself (though it can contain variables)",
                "the `loop_control` value must be specified as a dictionary and cannot be a \
                 variable itself (though it can contain variables)",
                "the `loop_control` value must be specified as a dictionary and cannot be a \
                 variable itself (though it can contain variables)",
                "'rescue' keyword cannot be used without 'block'",
                "'always' keyword cannot be used without 'block'",
                "'rescue' keyword cannot be used without 'block'",
                "Using a block as a handler is not supported.",
                "Using a block as a handler is not supported.",
                "Using a block as a handler is not supported.",
                "Using 'include_role' as a handler is not supported.",
                "Using 'ansible.builtin.import_role' as a handler is not supported.",
                "Using 'include_role' as a handler is not supported.",
                "Using 'include_role' as a handler is not supported.",
                "Cannot execute 'end_role' from outside of a role",
                "Cannot execute 'end_role' from outside of a role",
                "Cannot execute 'end_role' from a handler",
                "flush_handlers cannot be used as a handler",
                "flush_handlers cannot be used as a handler",
                "action and local_action are mutually exclusive",
                "action and local_action are mutually exclusive",
                "conflicting action statements: debug, frobnicate",
                "conflicting action statements: ansible.builtin.debug, nmae",
                "no module/action detected in task.",
                "no module/action detected in task.",
                "Found conflicting import_playbook actions: ansible.builtin.import_playbook, \
                 import_playbook",
                "playbook entries must be either valid plays or 'import_playbook' statements",
            ],
            "one diagnostic per BAD line, none for the GOOD ones — and none for `hosts: 42`, \
             the documented miss"
        );
        assert!(got.iter().all(|d| d.severity == Some(DiagnosticSeverity::ERROR)));
    }

    /// The one rule here that speaks where ansible-core does not: it carries its own code,
    /// so it can be toggled and suppressed separately, and it is a WARNING because the
    /// playbook does run — it just runs a loop the author did not write.
    #[test]
    fn the_shadowed_loop_warning_is_its_own_rule() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path = std::path::Path::new("../../demo/placement.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let got: Vec<_> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(|d| matches!(&d.code, Some(NumberOrString::String(s)) if s == "shadowed-loop"))
            .collect();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].severity, Some(DiagnosticSeverity::WARNING));
        // The message has to carry the mechanism, or it reads as a style nit.
        for want in ["discarded", "'items' lookup", "Delete one of the two"] {
            assert!(got[0].message.contains(want), "missing {want:?}: {}", got[0].message);
        }
    }

    /// T-155, end to end: the demo's two dead `loop_control:` blocks — one on a plain task,
    /// one on an `include_tasks` — each warn on their own code and name the inert keys. The
    /// block-level one does not, because that is T-107's invalid-attribute error.
    #[test]
    fn the_dead_loop_control_warning_is_its_own_rule() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path = std::path::Path::new("../../demo/placement.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let got: Vec<_> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(
                |d| matches!(&d.code, Some(NumberOrString::String(s)) if s == "dead-loop-control"),
            )
            .collect();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().all(|d| d.severity == Some(DiagnosticSeverity::WARNING)));
        assert!(got[0].message.contains("`loop_var`, `label` have no effect"), "{}", got[0].message);
        assert!(got[1].message.contains("`loop_var` has no effect"), "{}", got[1].message);

        // The Block's `loop_control` is the keyword rule's, not this one's.
        let blocks: Vec<String> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(|d| d.message.contains("not a valid attribute for a Block"))
            .map(|d| d.message)
            .collect();
        assert_eq!(blocks, ["'loop_control' is not a valid attribute for a Block"]);
    }

    /// T-110 row 24, the third rule here that speaks where ansible-core is silent: a
    /// `delegate_to:` next to a `local_action:` never applies, because `local_action` already
    /// set it to localhost. WARNING, since the play runs — it just runs somewhere else.
    #[test]
    fn the_discarded_delegate_to_warning_is_its_own_rule() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path = std::path::Path::new("../../demo/placement.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let got: Vec<_> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(
                |d| matches!(&d.code, Some(NumberOrString::String(s)) if s == "discarded-delegate-to"),
            )
            .collect();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].severity, Some(DiagnosticSeverity::WARNING));
        // The message has to name the mechanism, or it reads as a style nit.
        for want in ["localhost", "discarded", "runs locally"] {
            assert!(got[0].message.contains(want), "missing {want:?}: {}", got[0].message);
        }
    }

    /// Suppressible on its own id, and not by the replication rules' id — otherwise the two
    /// would be one rule wearing two names.
    #[test]
    fn the_discarded_delegate_to_warning_suppresses_on_its_own_id() {
        use tower_lsp::lsp_types::NumberOrString;
        let count = |text: &str| {
            let path = std::path::Path::new("../../demo/placement.yml")
                .canonicalize()
                .unwrap();
            let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
            super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter(|d| {
                    matches!(&d.code, Some(NumberOrString::String(s)) if s == "discarded-delegate-to")
                })
                .count()
        };
        let base = "- hosts: web\n  tasks:\n    - local_action: debug msg=y\n      delegate_to: other";
        assert_eq!(count(&format!("{base}\n")), 1);
        assert_eq!(count(&format!("{base} # noqa: discarded-delegate-to\n")), 0);
        assert_eq!(
            count(&format!("{base} # noqa: invalid-placement\n")),
            1,
            "invalid-placement must not silence discarded-delegate-to"
        );
    }

    /// The fourth rule here whose message is ours: `import_playbook:` in a task list. Ansible
    /// fails at run time with a message about parameters that never mentions position, and
    /// which one you get depends on the value's shape — so there is nothing to borrow.
    #[test]
    fn the_misplaced_import_playbook_rule_is_its_own() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path = std::path::Path::new("../../demo/placement.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let got: Vec<_> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(|d| {
                matches!(&d.code, Some(NumberOrString::String(s)) if s == "misplaced-import-playbook")
            })
            .collect();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().all(|d| d.severity == Some(DiagnosticSeverity::ERROR)));
        // It has to name the fix, since ansible's own message never mentions position.
        assert!(got[0].message.contains("import_tasks"), "{}", got[0].message);
    }

    /// A task-list entry that is not a mapping. Ansible refuses it, but reports the whole list
    /// instead of the offending item, always as `<class 'list'>`, with no line number — so this
    /// message is ours and carries its own id.
    #[test]
    fn the_malformed_task_entry_rule_is_its_own() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path = std::path::Path::new("../../demo/placement.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let got: Vec<_> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(
                |d| matches!(&d.code, Some(NumberOrString::String(s)) if s == "malformed-task-entry"),
            )
            .collect();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().all(|d| d.severity == Some(DiagnosticSeverity::ERROR)));
        // Each points at its own entry, which is the thing ansible cannot do at all.
        assert_ne!(got[0].range.start.line, got[1].range.start.line);
    }

    /// T-110 rows 5 and 23, both spellings. The messages differ only where ansible's behaviour
    /// does: the error is identical for import and include (measured — the include one just
    /// arrives at run time), while the empty-file warning says which of the two the author
    /// would ever have been told about.
    #[test]
    fn include_target_diagnostics_cover_both_spellings() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path = std::path::Path::new("../../demo/include_targets.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let got: Vec<_> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(|d| {
                matches!(&d.code, Some(NumberOrString::String(s))
                    if s == "empty-task-file" || s == "invalid-task-file")
            })
            .collect();

        let seen: Vec<(&str, &str)> = got
            .iter()
            .map(|d| {
                let Some(NumberOrString::String(id)) = &d.code else {
                    unreachable!()
                };
                (id.as_str(), d.message.as_str())
            })
            .collect();
        assert_eq!(
            seen,
            [
                (
                    "empty-task-file",
                    "the file this imports is empty — no tasks come from it. Ansible warns and \
                     carries on."
                ),
                (
                    "empty-task-file",
                    "the file this includes is empty — no tasks come from it. Ansible says \
                     nothing at all."
                ),
                ("invalid-task-file", "included task files must contain a list of tasks"),
                ("invalid-task-file", "included task files must contain a list of tasks"),
            ],
            "{got:?}"
        );
        // The empty file is a warning either way; the wrong shape is fatal either way.
        assert_eq!(got[0].severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(got[1].severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(got[2].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(got[3].severity, Some(DiagnosticSeverity::ERROR));
        // Anchored on the reference in *this* file, never in the file at fault — it may not
        // even be open. The templated include at the end has no target, so it says nothing.
        assert!(got.iter().all(|d| d.range.start.line < 40));
    }

    /// Every rule id this ticket owns. A fixture that stops exercising one of these has lost
    /// coverage silently, which is what the last assertion below is for.
    const T110_RULES: &[&str] = &[
        "invalid-placement",
        "shadowed-loop",
        "dead-loop-control",
        "dead-loop-on-meta",
        "discarded-delegate-to",
        "misplaced-import-playbook",
        "malformed-task-entry",
        "reserved-tag-name",
        "invalid-tag-member",
        "empty-task-file",
        "invalid-task-file",
        "empty-playbook",
        "invalid-playbook",
    ];

    /// T-110's fixture box: the demo files are the fixture, and their `# GOOD` / `# BAD`
    /// annotations are the expected result. This turns those comments into a contract.
    ///
    /// The GOOD half is the point. Every other test here asserts that a rule *fires*; nothing
    /// asserted that the legal spelling beside it stays quiet, so a rule that over-fired on
    /// correct input would have passed the whole suite. Each `# GOOD` line is a case measured
    /// clean against real `ansible-playbook`, so a diagnostic on one is a false positive by
    /// construction.
    ///
    /// Only this ticket's rule ids count. A `# GOOD here:` line may legitimately carry another
    /// rule's diagnostic — `loop_control:` on a block is T-107's `invalid-attribute`, and the
    /// comment says so — and that is coverage, not a conflict.
    #[test]
    fn every_annotated_demo_line_matches_its_diagnostics() {
        use tower_lsp::lsp_types::NumberOrString;
        let mut seen_rules: std::collections::BTreeSet<String> = Default::default();
        for name in ["placement.yml", "include_targets.yml"] {
            let path = std::path::Path::new("../../demo").join(name).canonicalize().unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            let a = super::Backend::analyze_text(text.clone(), &path).unwrap();
            let ours: Vec<u32> = super::Backend::diagnostics_of(&a)
                .iter()
                .filter_map(|d| match &d.code {
                    Some(NumberOrString::String(s)) if T110_RULES.contains(&s.as_str()) => {
                        seen_rules.insert(s.clone());
                        Some(d.range.start.line)
                    }
                    _ => None,
                })
                .collect();
            let flagged = |line: usize| ours.contains(&(line as u32));

            let src: Vec<&str> = text.lines().collect();
            for (i, line) in src.iter().enumerate() {
                let bad = line.contains("# BAD") || line.contains("# WARN");
                let good = line.contains("# GOOD");
                // An annotation written on its own comment line heads the statement below it —
                // used where the explanation is too long to sit at the end of the code line.
                let target = if line.trim_start().starts_with('#') {
                    src.iter().skip(i + 1).position(|l| !l.trim_start().starts_with('#')).map(
                        |off| i + 1 + off,
                    )
                } else {
                    Some(i)
                };
                let Some(target) = target else { continue };
                if bad {
                    assert!(
                        flagged(target),
                        "{name}:{} is marked BAD and produced no diagnostic: {}",
                        target + 1,
                        src[target].trim()
                    );
                }
                if good && !bad {
                    assert!(
                        !flagged(target),
                        "{name}:{} is marked GOOD and was flagged anyway: {}",
                        target + 1,
                        src[target].trim()
                    );
                }
            }
        }
        let missing: Vec<&&str> =
            T110_RULES.iter().filter(|r| !seen_rules.contains(**r)).collect();
        assert!(missing.is_empty(), "no fixture line exercises {missing:?}");
    }


    /// No false positives: every demo file except the one built to demonstrate the rule
    /// The picker's offer for the demo, pinned. `demo/` labels its inventories with what
    /// they do, and rule 4 says a label is a claim — this is the claim.
    ///
    /// The two negatives carry the weight. `demo/` itself must NOT be offered as a folder
    /// (three inventories among a dozen playbooks; `-i demo` would feed every playbook to
    /// the inventory parser), and `README.md` must not appear among a folder's `reads`.
    #[test]
    fn the_picker_offers_the_demo_folders_and_not_the_demo_itself() {
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let got = super::Backend::inventory_candidates(Some(&demo));
        let path_of = |c: &serde_json::Value| c["path"].as_str().unwrap_or("").to_string();

        let folders: Vec<String> =
            got.iter().filter(|c| c["dir"] == true).map(path_of).collect();
        assert_eq!(folders, ["inventories/prod", "inventories/staging"]);

        let files: Vec<String> =
            got.iter().filter(|c| c["dir"] != true).map(path_of).collect();
        // A file inside an offered folder is covered by that folder's row; offering it
        // again doubled the list for no extra choice.
        assert!(
            !files.iter().any(|f| f.starts_with("inventories/")),
            "a folder's own files were offered separately: {files:?}"
        );
        // A Jinja list indented inside a folded scalar is not an INI section header.
        assert!(
            !files.iter().any(|f| f == "tasks/lenient_scalar.yml"),
            "a task file was offered as an inventory: {files:?}"
        );
        for want in
            ["inventory-prod.ini", "inventory-lab.yml", "inventory-dynamic.yml", "inventory-toml.toml"]
        {
            assert!(files.contains(&want.to_string()), "{want} missing from {files:?}");
        }
        // A playbook is not an inventory, however many `hosts:` keys it has.
        assert!(!files.iter().any(|f| f == "hostvars.yml"), "a playbook was offered: {files:?}");

        let prod = got.iter().find(|c| path_of(c) == "inventories/prod").unwrap();
        let reads: Vec<&str> =
            prod["reads"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        // Measured with `ansible-inventory -i demo/inventories/prod --list`: both `.ini`
        // files contributed hosts, `group_vars/all.yml` reached every host as a variable
        // without being a source of its own, and README.md contributed nothing.
        assert_eq!(reads, ["inventories/prod/db.ini", "inventories/prod/hosts.ini"]);
    }

    /// stays free of placement diagnostics.
    #[test]
    fn every_other_demo_file_is_free_of_placement_diagnostics() {
        use tower_lsp::lsp_types::NumberOrString;
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let files = ansible_core::workspace::yaml_files(&demo);
        assert!(files.len() > 10, "the demo walk found the demo");
        for path in files {
            if path.file_name().is_some_and(|n| n == "placement.yml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Some(a) = super::Backend::analyze_text(text, &path) else { continue };
            let bad: Vec<String> = super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter(|d| {
                    matches!(&d.code, Some(NumberOrString::String(s)) if s == "invalid-placement")
                })
                .map(|d| d.message)
                .collect();
            assert!(bad.is_empty(), "{}: {bad:?}", path.display());
        }
    }

    use ansible_core::cache::ScanCache;

    fn msgs(ds: &[tower_lsp::lsp_types::Diagnostic]) -> Vec<String> {
        ds.iter().map(|d| d.message.clone()).collect()
    }

    /// T-062 box 8 on the demo: the unknown host is flagged, the real one beside it is not.
    ///
    /// `demo/hostvars.yml` reads `web01` (in inventory-lab.yml) and `web0143` (not), in the
    /// same file, through the same syntax. Nothing separates them but the host list, which is
    /// the claim.
    #[test]
    fn the_demo_flags_the_unknown_host_and_not_the_real_one() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path = std::path::Path::new("../../demo/hostvars.yml").canonicalize().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text.clone(), &path).unwrap();
        let ds = super::Backend::unknown_host_diagnostics(&a, &path, &ScanCache::default());

        assert_eq!(ds.len(), 1, "expected exactly the one bad host: {:?}", msgs(&ds));
        assert_eq!(ds[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(ds[0].code, Some(NumberOrString::String("unknown-host".into())));
        assert!(ds[0].message.contains("web0143"), "the host is not named: {}", ds[0].message);

        // The range covers the host name only — not the quotes, not `hostvars[`.
        let line = text.lines().nth(ds[0].range.start.line as usize).unwrap();
        let (s, e) = (ds[0].range.start.character as usize, ds[0].range.end.character as usize);
        assert_eq!(&line[s..e], "web0143");
    }

    /// Every reason to stay quiet, each asserted against a case that fires without it.
    ///
    /// The control is the first line of each pair: the same file, the same read, one thing
    /// changed. A silence test that never saw the rule fire proves only that the rule is off.
    #[test]
    fn unknown_host_is_silent_wherever_the_host_list_is_not_knowable() {
        let d = std::env::temp_dir().join("ansible-lsp-t062-box8");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("hosts.ini"), "[web]\nweb01\n").unwrap();
        std::fs::write(d.join("dyn.yml"), "plugin: amazon.aws.aws_ec2\nregions: [us-east-1]\n")
            .unwrap();
        let play = d.join("play.yml");

        let fires = |text: &str, inventory: Vec<std::path::PathBuf>| {
            std::fs::write(&play, text).unwrap();
            let a = super::Backend::analyze_text(text.to_string(), &play).unwrap();
            let cache = ScanCache::default().with_inventory(inventory);
            super::Backend::unknown_host_diagnostics(&a, &play, &cache).len()
        };
        let ini = || vec![d.join("hosts.ini")];
        let read = "- hosts: all\n  tasks:\n    - debug:\n        msg: \"{{ hostvars['nope'].x }}\"\n";

        // The control. Everything below changes exactly one thing about this.
        assert_eq!(fires(read, ini()), 1, "control: an unknown host against a read inventory");
        assert_eq!(fires(read, vec![]), 0, "no inventory resolved");
        assert_eq!(fires(read, vec![d.join("dyn.yml")]), 0, "a declined dynamic inventory");
        assert_eq!(fires(read, vec![d.join("missing.ini")]), 0, "an inventory that is not there");

        // A real host, same inventory.
        let ok = read.replace("nope", "web01");
        assert_eq!(fires(&ok, ini()), 0, "a host the inventory declares");

        // The implicit localhost, all three spellings. Measured against an inventory holding
        // only `web01`: each is a member and each resolves. Shipping with just `localhost`
        // escaped left the other two as false errors on working code.
        for implicit in ["localhost", "127.0.0.1", "::1"] {
            assert_eq!(fires(&read.replace("nope", implicit), ini()), 0, "implicit {implicit}");
        }

        // An expression that swallows the undefined. `hostvars['nope'].x` is fatal, but
        // `… | default('z')` prints `z` — so the message's "fails at runtime" would be false.
        let defaulted = read.replace(".x }}", ".x | default('z') }}");
        assert_eq!(fires(&defaulted, ini()), 0, "| default() rescues the read");
        let tested = read.replace(".x }}", ".x if hostvars['nope'] is defined else '' }}");
        assert_eq!(fires(&tested, ini()), 0, "is defined rescues the read");
        // ...but a default belonging to a *different* expression rescues nothing.
        let other = read.replace(".x }}\"", ".x }} {{ y | default(1) }}\"");
        assert_eq!(fires(&other, ini()), 1, "a default in a neighbouring expression");

        // add_host invents hosts at runtime; one anywhere in the file silences the file.
        let added = format!("{read}    - add_host:\n        name: nope\n");
        assert_eq!(fires(&added, ini()), 0, "add_host anywhere in the file");

        // A templated key names no host we can know.
        let templated = read.replace("'nope'", "some_var");
        assert_eq!(fires(&templated, ini()), 0, "a templated subscript");

        // The idiomatic "first host of a group". The quoted literal here is a GROUP name,
        // belonging to the inner `groups[...]`, and the host it resolves to is unknowable.
        // This one shape produced 20 of the 21 hits the rule first reported against the
        // reference corpus, every one of them working code.
        let via_group = read.replace("'nope'", "groups['web'][0]");
        assert_eq!(fires(&via_group, ini()), 0, "hostvars[groups['web'][0]]");
        // ...and the group name is not quietly accepted as a host either.
        let group_named = read.replace("'nope'", "'web'");
        assert_eq!(fires(&group_named, ini()), 1, "a group name is not a host");

        // In a comment it is prose, not a read — 2 of the corpus's 23 uses look like this.
        let commented: String =
            read.lines().map(|l| format!("# {l}\n")).chain(["- hosts: all\n".into()]).collect();
        assert_eq!(fires(&commented, ini()), 0, "inside a comment");

        // And the ordinary escape hatch.
        let noqa = read.replace(".x }}\"", ".x }}\" # noqa: unknown-host");
        assert_eq!(fires(&noqa, ini()), 0, "# noqa");
    }

    /// T-179's fixture, asserted as the exact set rather than a count.
    ///
    /// The two rows that carry it: `buildbox` comes from an imported file's `add_host` and
    /// must be silent, while `web0143` in the same file must still fire. A "silence the whole
    /// file when anything reachable calls add_host" implementation passes the first and fails
    /// the second, which is the easy wrong fix this pins shut.
    #[test]
    fn an_imported_add_host_names_a_host_without_silencing_the_file() {
        let path =
            std::path::Path::new("../../demo/unknown_host.yml").canonicalize().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let ds = super::Backend::unknown_host_diagnostics(&a, &path, &ScanCache::default());
        let named: Vec<&str> =
            ds.iter().filter_map(|d| d.message.split('`').nth(1)).collect();
        assert_eq!(named, ["web0143"], "expected only the ghost host: {:?}", msgs(&ds));
    }

    /// The other half of T-179: when a reachable `add_host` name is **templated**, the
    /// created set is not enumerable and the rule must stop answering for the file —
    /// including for a host nothing creates. Answering from the half we can read is exactly
    /// the under-matching that produced the bug.
    #[test]
    fn a_templated_add_host_anywhere_reachable_silences_the_file() {
        let d = std::env::temp_dir().join("ansible-lsp-t179");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("tasks")).unwrap();
        std::fs::write(d.join("ansible.cfg"), "[defaults]\ninventory = ./hosts.ini\n").unwrap();
        std::fs::write(d.join("hosts.ini"), "[web]\nweb01\n").unwrap();
        let play = d.join("play.yml");
        let read = "- hosts: web\n  tasks:\n    - import_tasks: tasks/make.yml\n    \
                    - debug:\n        msg: \"{{ hostvars['ghost'].x }}\"\n";
        std::fs::write(&play, read).unwrap();

        let fires = |made: &str| {
            std::fs::write(d.join("tasks/make.yml"), made).unwrap();
            let a = super::Backend::analyze_text(read.to_string(), &play).unwrap();
            super::Backend::unknown_host_diagnostics(&a, &play, &ScanCache::default()).len()
        };

        // Control: a literal name leaves the rule live, so `ghost` is still reported.
        assert_eq!(fires("- add_host:\n    name: realhost\n"), 1, "control: literal name");
        // A templated name over a LITERAL loop is readable — substitute and the iteration is
        // enumerable, so the rule stays live and `ghost` is still reported. Measured: this
        // really does create one host per item, suffix included.
        assert_eq!(
            fires("- add_host:\n    name: \"{{ item }}\"\n  loop: ['la', 'lb']\n"),
            1,
            "a literal loop is enumerable"
        );
        assert_eq!(
            fires("- add_host:\n    name: \"{{ item }}-web\"\n  loop: ['la']\n"),
            1,
            "a suffix survives substitution"
        );

        // Templated over something unreadable: nothing can be named, so nothing is claimed.
        // Each of these leaves a `{{` behind after substitution, which is the one guard.
        assert_eq!(fires("- add_host:\n    name: \"{{ item }}\"\n  loop: \"{{ found }}\"\n"), 0);
        assert_eq!(fires("- add_host:\n    name: \"{{ item }}\"\n  with_items: [a]\n"), 0);
        assert_eq!(
            fires("- add_host:\n    name: \"{{ item }}-{{ env }}\"\n  loop: ['la']\n"),
            0,
            "a second variable in the name is not resolved by the loop"
        );
        assert_eq!(
            fires(
                "- add_host:\n    name: \"{{ node }}\"\n  loop: ['la']\n  \
                 loop_control:\n    loop_var: node\n"
            ),
            0,
            "loop_var renames item; unhandled, and the guard makes that silence not a lie"
        );
        // The free-form spelling and the `host:`/`hostname:` aliases are all readable, and
        // all three were measured to create their host. Each names a host, so `ghost` beside
        // it is still reported — silence here would mean the name was not read.
        assert_eq!(fires("- add_host: name=freeform\n"), 1, "free-form args are parsed");
        assert_eq!(fires("- add_host:\n    host: aliased\n"), 1, "the host: alias");
        assert_eq!(fires("- add_host:\n    hostname: aliased\n"), 1, "the hostname: alias");
    }

    /// Every edge kind that can carry an `add_host`, and the templated filename that cannot.
    ///
    /// The role row is measured, not assumed: `roles:` runs before the play's `tasks:`, and a
    /// role's `add_host` really does reach them — verified against 2.21.2 before this was
    /// written, with the negative control that an `add_host` in a file nothing includes stays
    /// invisible.
    #[test]
    fn add_host_is_followed_through_roles_and_both_include_forms() {
        let d = std::env::temp_dir().join("ansible-lsp-t179-edges");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("tasks")).unwrap();
        std::fs::create_dir_all(d.join("roles/maker/tasks")).unwrap();
        std::fs::write(d.join("ansible.cfg"), "[defaults]\ninventory = ./hosts.ini\n").unwrap();
        std::fs::write(d.join("hosts.ini"), "[web]\nweb01\n").unwrap();
        std::fs::write(
            d.join("roles/maker/tasks/main.yml"),
            "- add_host:\n    name: rolehost\n",
        )
        .unwrap();
        std::fs::write(d.join("tasks/make.yml"), "- add_host:\n    name: filehost\n").unwrap();
        std::fs::write(d.join("tasks/other.yml"), "- debug:\n    msg: hi\n").unwrap();

        let play = d.join("play.yml");
        let fires = |body: &str, host: &str| {
            let text = format!(
                "- hosts: web\n{body}  post_tasks:\n    - debug:\n        \
                 msg: \"{{{{ hostvars['{host}'].x }}}}\"\n"
            );
            std::fs::write(&play, &text).unwrap();
            let a = super::Backend::analyze_text(text, &play).unwrap();
            super::Backend::unknown_host_diagnostics(&a, &play, &ScanCache::default()).len()
        };

        let role = "  roles:\n    - maker\n";
        let import = "  tasks:\n    - import_tasks: tasks/make.yml\n";
        let include = "  tasks:\n    - include_tasks: tasks/make.yml\n";
        let templated = "  tasks:\n    - include_tasks: \"{{ kind }}.yml\"\n";
        let unrelated = "  tasks:\n    - import_tasks: tasks/other.yml\n";

        assert_eq!(fires(role, "rolehost"), 0, "a role's add_host counts");
        assert_eq!(fires(import, "filehost"), 0, "import_tasks");
        assert_eq!(fires(include, "filehost"), 0, "include_tasks is followed too");

        // The controls. Without these the three above are satisfied by a rule that never
        // fires at all.
        assert_eq!(fires(role, "ghost"), 1, "control: the rule is alive through a role");
        assert_eq!(fires(import, "ghost"), 1, "control: alive through import_tasks");
        assert_eq!(
            fires(unrelated, "filehost"),
            1,
            "an add_host in a file this play never includes must not count"
        );

        // A templated filename is a hole in the graph: whatever it resolves to may create
        // hosts, so nothing can be claimed for the file. Not permanent — T-180 would close
        // it by reading an assert that constrains `kind`.
        assert_eq!(fires(templated, "ghost"), 0, "a templated include edge");
    }

    /// Several reads in one file are judged one at a time, and the rule reaches every place
    /// an expression can sit — not just `msg:`.
    #[test]
    fn every_read_in_a_file_is_judged_on_its_own() {
        let d = std::env::temp_dir().join("ansible-lsp-t062-many");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("ansible.cfg"), "[defaults]\ninventory = ./hosts.ini\n").unwrap();
        std::fs::write(d.join("hosts.ini"), "[web]\nreal1\nreal2\n").unwrap();
        let play = d.join("play.yml");

        let named = |text: &str| -> Vec<String> {
            std::fs::write(&play, text).unwrap();
            let a = super::Backend::analyze_text(text.to_string(), &play).unwrap();
            super::Backend::unknown_host_diagnostics(&a, &play, &ScanCache::default())
                .iter()
                .filter_map(|d| d.message.split('`').nth(1).map(str::to_owned))
                .collect()
        };

        // Four reads, two of them real. Only the two ghosts are named, and the good ones
        // between them do not shift or suppress the verdicts.
        let mixed = "- hosts: all\n  tasks:\n    - debug:\n        msg: >-\n          \
                     {{ hostvars['real1'].a }} {{ hostvars['nope1'].b }}\n          \
                     {{ hostvars['real2'].c }} {{ hostvars['nope2'].d }}\n";
        assert_eq!(named(mixed), ["nope1", "nope2"]);

        // A `when:` is the same expression language — a rule that only read task args would
        // miss it, and the read is just as fatal there.
        let cond = "- hosts: all\n  tasks:\n    - debug:\n        msg: hi\n      \
                    when: hostvars['nope3'].ready\n";
        assert_eq!(named(cond), ["nope3"]);

        // A play-level `vars:` value, which is neither a task arg nor a condition.
        let pv = "- hosts: all\n  vars:\n    leader: \"{{ hostvars['nope4'].ip }}\"\n  tasks: []\n";
        assert_eq!(named(pv), ["nope4"]);

        // `# noqa` reaches the same line and the one immediately before it, and no further —
        // `Document::is_suppressed`'s rule, shared by every rule in the tool. Two lines up,
        // with `- debug:` in between, it does not apply, and that is worth pinning because a
        // reader would expect a comment heading the task to cover the whole task.
        let two_above = "- hosts: all\n  tasks:\n    # noqa: unknown-host\n    - debug:\n        \
                         msg: \"{{ hostvars['nope5'].x }}\"\n";
        assert_eq!(named(two_above), ["nope5"], "two lines up is out of reach");
        let one_above = "- hosts: all\n  tasks:\n    - debug:\n        # noqa: unknown-host\n        \
                         msg: \"{{ hostvars['nope5'].x }}\"\n";
        assert!(named(one_above).is_empty(), "the line immediately above does apply");
        let trailing = "- hosts: all\n  tasks:\n    - debug:\n        \
                        msg: \"{{ hostvars['nope6'].x }}\" # noqa: unknown-host\n";
        assert!(named(trailing).is_empty(), "a trailing noqa must silence it");

        // A different rule's noqa does not silence this one.
        let other = "- hosts: all\n  tasks:\n    - debug:\n        \
                     msg: \"{{ hostvars['nope7'].x }}\" # noqa: var-undefined\n";
        assert_eq!(named(other), ["nope7"]);
    }

    /// The host set reaches the rule from every source that can supply one, and the two
    /// contributors are checked independently rather than as one blob.
    ///
    /// The range row is the integration the unit tests do not cover: `expand_host_pattern`
    /// is asserted on its own, but nothing until now checked that an expanded host actually
    /// arrives at the diagnostic — a reader that dropped the expansion would pass every
    /// range test and still red-flag `web03`.
    #[test]
    fn a_host_counts_from_whichever_source_supplies_it() {
        let d = std::env::temp_dir().join("ansible-lsp-t062-sources");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a.ini"), "[web]\nweb[01:03]\n").unwrap();
        std::fs::write(d.join("b.yml"), "all:\n  hosts:\n    fromsecond:\n").unwrap();
        std::fs::write(d.join("c.toml"), "[web.hosts.fromtoml]\nip = \"1\"\n").unwrap();
        std::fs::write(d.join("huge.ini"), "[web]\nbig[1:1000000]\n").unwrap();
        let play = d.join("play.yml");

        let fires = |host: &str, inv: Vec<std::path::PathBuf>| {
            let text = format!(
                "- hosts: all\n  tasks:\n    - debug:\n        msg: \"{{{{ hostvars['{host}'].x }}}}\"\n"
            );
            std::fs::write(&play, &text).unwrap();
            let a = super::Backend::analyze_text(text, &play).unwrap();
            let cache = ScanCache::default().with_inventory(inv);
            super::Backend::unknown_host_diagnostics(&a, &play, &cache).len()
        };
        let all = || vec![d.join("a.ini"), d.join("b.yml"), d.join("c.toml")];

        // A range-expanded host arrives at the rule, ends included.
        for h in ["web01", "web02", "web03"] {
            assert_eq!(fires(h, all()), 0, "{h} came from the expansion");
        }
        assert_eq!(fires("web04", all()), 1, "control: one past the range is still unknown");

        // Each source contributes, including the second and third of a list.
        assert_eq!(fires("fromsecond", all()), 0, "a yaml source later in the list");
        assert_eq!(fires("fromtoml", all()), 0, "a toml source later in the list");

        // Host names are case-sensitive; `WEB01` is not `web01`.
        assert_eq!(fires("WEB01", all()), 1, "host names are not case-folded");

        // A pattern past MAX_PATTERN_HOSTS makes the whole list unknowable, so the rule stops
        // answering — never a truncated list, which would report every host past the cut.
        assert_eq!(fires("anything", vec![d.join("huge.ini")]), 0, "over the cap is unknowable");
        assert_eq!(
            fires("anything", vec![d.join("a.ini"), d.join("huge.ini")]),
            0,
            "one uncountable source poisons the others"
        );
    }

    /// Edges the walk must follow that the earlier test does not reach: `import_playbook`,
    /// and a role's non-`main.yml` task file via `tasks_from`.
    #[test]
    fn add_host_is_followed_through_import_playbook_and_tasks_from() {
        let d = std::env::temp_dir().join("ansible-lsp-t179-edges2");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("roles/maker/tasks")).unwrap();
        std::fs::write(d.join("ansible.cfg"), "[defaults]\ninventory = ./hosts.ini\n").unwrap();
        std::fs::write(d.join("hosts.ini"), "[web]\nweb01\n").unwrap();
        std::fs::write(
            d.join("roles/maker/tasks/extra.yml"),
            "- add_host:\n    name: extrahost\n",
        )
        .unwrap();
        std::fs::write(
            d.join("inner.yml"),
            "- hosts: web\n  tasks:\n    - add_host:\n        name: innerhost\n",
        )
        .unwrap();

        let play = d.join("play.yml");
        let fires = |body: &str, host: &str| {
            let text = format!(
                "{body}- hosts: web\n  tasks:\n    - debug:\n        \
                 msg: \"{{{{ hostvars['{host}'].x }}}}\"\n"
            );
            std::fs::write(&play, &text).unwrap();
            let a = super::Backend::analyze_text(text, &play).unwrap();
            super::Backend::unknown_host_diagnostics(&a, &play, &ScanCache::default()).len()
        };

        let imported = "- import_playbook: inner.yml\n";
        assert_eq!(fires(imported, "innerhost"), 0, "import_playbook carries its add_host");
        assert_eq!(fires(imported, "ghost"), 1, "control: alive through import_playbook");

        let from = "- hosts: web\n  tasks:\n    - include_role:\n        name: maker\n        \
                    tasks_from: extra.yml\n";
        assert_eq!(fires(from, "extrahost"), 0, "tasks_from reaches a role's other file");
        assert_eq!(fires(from, "ghost"), 1, "control: alive through tasks_from");
    }

    /// An `add_host` inside an include **cycle** still names its host.
    ///
    /// The walk truncates on a cycle (`walk.truncated`) and returns without merging that
    /// frame, which is what makes it terminate. The question this pins is whether the
    /// truncation eats the host: a lost name reads as "no such host" and becomes a red ERROR
    /// on working code, the same failure T-179 was. Reasoning says the cycle member's own
    /// tasks are collected before it re-enters, but that is exactly the kind of reasoning
    /// this file exists to distrust.
    #[test]
    fn a_cycle_in_the_include_graph_does_not_lose_the_host_it_creates() {
        let d = std::env::temp_dir().join("ansible-lsp-t179-cycle");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("tasks")).unwrap();
        std::fs::write(d.join("ansible.cfg"), "[defaults]\ninventory = ./hosts.ini\n").unwrap();
        std::fs::write(d.join("hosts.ini"), "[web]\nweb01\n").unwrap();
        // a -> b -> a, with the add_host on the far side of the loop.
        std::fs::write(
            d.join("tasks/a.yml"),
            "- import_tasks: b.yml\n",
        )
        .unwrap();
        std::fs::write(
            d.join("tasks/b.yml"),
            "- add_host:\n    name: cyclehost\n- import_tasks: a.yml\n",
        )
        .unwrap();

        let play = d.join("play.yml");
        let fires = |host: &str| {
            let text = format!(
                "- hosts: web\n  tasks:\n    - import_tasks: tasks/a.yml\n    - debug:\n        \
                 msg: \"{{{{ hostvars['{host}'].x }}}}\"\n"
            );
            std::fs::write(&play, &text).unwrap();
            let a = super::Backend::analyze_text(text, &play).unwrap();
            super::Backend::unknown_host_diagnostics(&a, &play, &ScanCache::default()).len()
        };
        assert_eq!(fires("cyclehost"), 0, "the cycle swallowed the host it creates");
        assert_eq!(fires("ghost"), 1, "control: the rule survives the cycle at all");
    }

    /// The false-positive gate. A wrong ERROR here is worse than the missing feature, so
    /// every other demo file must stay clean with the demo's own inventory in effect.
    #[test]
    fn every_other_demo_file_is_free_of_unknown_host_diagnostics() {
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let files = ansible_core::workspace::yaml_files(&demo);
        assert!(files.len() > 10, "the demo walk found the demo");
        for path in files {
            if path
                .file_name()
                .is_some_and(|n| n == "hostvars.yml" || n == "unknown_host.yml")
            {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Some(a) = super::Backend::analyze_text(text, &path) else { continue };
            let bad =
                msgs(&super::Backend::unknown_host_diagnostics(&a, &path, &ScanCache::default()));
            assert!(bad.is_empty(), "{}: {bad:?}", path.display());
        }
    }

    /// T-062 box 7, both halves, as the demo labels them.
    ///
    /// `inventories/prod/group_vars/all.yml` is marked WARN and must fire; `inventory-lab.yml`
    /// is marked NO HINT and must not.
    ///
    /// The second half is quiet because the key is nested under a group's `vars:`, which is
    /// the only place a YAML inventory can carry it — not because the file is an inventory.
    /// The directory rule is not what saves it, so the synthetic case below carries that: a
    /// flat top-level key outside a vars directory, which nothing but the path excludes.
    #[test]
    fn the_demo_flags_group_priority_only_where_it_is_inert() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let fired = |rel: &str| {
            let path = demo.join(rel);
            let text = std::fs::read_to_string(&path).unwrap();
            let a = super::Backend::analyze_text(text, &path).unwrap();
            super::Backend::group_priority_diagnostics(&a, &path)
        };

        let warn = fired("inventories/prod/group_vars/all.yml");
        assert_eq!(warn.len(), 1, "the WARN row did not fire");
        assert_eq!(warn[0].severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(
            warn[0].code,
            Some(NumberOrString::String("group-priority-ignored".into()))
        );
        assert!(warn[0].message.contains("[<group>:vars]"), "no working spelling offered");

        assert!(fired("inventory-lab.yml").is_empty(), "flagged a YAML inventory, where it works");

        // The directory half, which the demo pair does not exercise. Same flat text as the
        // WARN row, one directory over.
        let flat = "ansible_group_priority: 10\n".to_string();
        let outside = std::path::Path::new("/p/vars/common.yml");
        let a = super::Backend::analyze_text(flat, outside).unwrap();
        assert!(super::Backend::group_priority_diagnostics(&a, outside).is_empty());
    }

    /// `# noqa` on the key, since a user who knows it is dead may still want it recorded.
    #[test]
    fn group_priority_is_suppressible() {
        let path = std::path::Path::new("/p/group_vars/all.yml");
        let text = "ansible_group_priority: 10 # noqa: group-priority-ignored\n".to_string();
        let a = super::Backend::analyze_text(text, path).unwrap();
        assert!(super::Backend::group_priority_diagnostics(&a, path).is_empty());
    }

    /// The false-positive gate. `ansible_group_priority` is a real key people write, and the
    /// demo carries `group_vars`/`host_vars` trees for other rules — this rule must not start
    /// firing in any of them.
    #[test]
    fn every_other_demo_file_is_free_of_group_priority_diagnostics() {
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let files = ansible_core::workspace::yaml_files(&demo);
        assert!(files.len() > 10, "the demo walk found the demo");
        for path in files {
            if path.ends_with("inventories/prod/group_vars/all.yml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Some(a) = super::Backend::analyze_text(text, &path) else { continue };
            let bad: Vec<String> = super::Backend::group_priority_diagnostics(&a, &path)
                .into_iter()
                .map(|d| d.message)
                .collect();
            assert!(bad.is_empty(), "{}: {bad:?}", path.display());
        }
    }

    /// The same guard for rows 5 and 23. It matters more here than for the single-document
    /// rules: this one reads a *second* file, so a bad verdict on any legitimate import in the
    /// demo — a role's `tasks/main.yml`, a sibling task file — would show up as a false error
    /// on a file nobody touched.
    #[test]
    fn every_other_demo_file_is_free_of_include_target_diagnostics() {
        use tower_lsp::lsp_types::NumberOrString;
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let files = ansible_core::workspace::yaml_files(&demo);
        assert!(files.len() > 10, "the demo walk found the demo");
        for path in files {
            if path.file_name().is_some_and(|n| n == "include_targets.yml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Some(a) = super::Backend::analyze_text(text, &path) else { continue };
            let bad: Vec<String> = super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter(|d| {
                    matches!(&d.code, Some(NumberOrString::String(s))
                        if s == "empty-task-file" || s == "invalid-task-file")
                })
                .map(|d| d.message)
                .collect();
            assert!(bad.is_empty(), "{}: {bad:?}", path.display());
        }
    }

    /// T-100's false-positive gate, and the one that matters most for this rule: role
    /// params are the documented way to pass values into a role, so every `roles:` entry
    /// in the demo that is doing something legitimate must stay silent.
    #[test]
    fn every_other_demo_file_is_free_of_role_param_diagnostics() {
        use tower_lsp::lsp_types::NumberOrString;
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let files = ansible_core::workspace::yaml_files(&demo);
        assert!(files.len() > 10, "the demo walk found the demo");
        for path in files {
            if path.file_name().is_some_and(|n| n == "role_params.yml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Some(a) = super::Backend::analyze_text(text, &path) else { continue };
            let bad: Vec<String> = super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter(|d| {
                    matches!(&d.code, Some(NumberOrString::String(s))
                        if s == ansible_core::attributes::ROLE_PARAM_RULE_ID)
                })
                .map(|d| d.message)
                .collect();
            assert!(bad.is_empty(), "{}: {bad:?}", path.display());
        }
    }

    /// T-100: the demo fixture's BAD rows, and only those. Pins which keys the rule owns
    /// — the GOOD and SILENCED rows in the same file are the other half of the assertion.
    #[test]
    fn the_role_params_demo_reports_exactly_its_bad_rows() {
        use tower_lsp::lsp_types::NumberOrString;
        let path = std::path::Path::new("../../demo")
            .canonicalize()
            .unwrap()
            .join("role_params.yml");
        let text = std::fs::read_to_string(&path).expect("demo fixture");
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let msgs: Vec<String> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(|d| {
                matches!(&d.code, Some(NumberOrString::String(s))
                    if s == ansible_core::attributes::ROLE_PARAM_RULE_ID)
            })
            .map(|d| d.message)
            .collect();
        let keys: Vec<&str> = msgs.iter().map(|m| m.split('\'').nth(1).unwrap()).collect();
        assert_eq!(keys, ["tasks_from", "becom_user", "register", "gather_facts"], "{msgs:?}");
    }

    /// Go-to-definition inside a `.j2`, through the real handler, over the demo's own chain —
    /// so the fixture and the feature are pinned by one test.
    ///
    /// The role row is the one carrying an ordering claim: `common.j2` sits at play level and
    /// its `{% include "shared.j2" %}` has three answers, but from a template *inside*
    /// `edge-proxy` the role's own copy is what a render picks. Measured on 2.21.2.
    #[tokio::test]
    async fn an_include_target_in_a_demo_template_jumps_to_the_file_ansible_finds() {
        use tower_lsp::LanguageServer;
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let service = lsp_service(scan_state(&demo));

        async fn jump(
            service: &tower_lsp::LspService<super::Backend>,
            path: &std::path::Path,
            needle: &str,
        ) -> Option<std::path::PathBuf> {
            let text = std::fs::read_to_string(path).expect("demo template");
            let doc = super::Document::new(text.clone());
            let byte = text.find(needle).unwrap_or_else(|| panic!("{needle:?} not in {path:?}"))
                + needle.len() / 2;
            let (line, col) = doc.byte_to_lsp(byte);
            let uri = tower_lsp::lsp_types::Url::from_file_path(path).unwrap();
            service
                .inner()
                .did_open(tower_lsp::lsp_types::DidOpenTextDocumentParams {
                    text_document: tower_lsp::lsp_types::TextDocumentItem {
                        uri: uri.clone(),
                        language_id: "jinja".into(),
                        version: 1,
                        text,
                    },
                })
                .await;
            let params = tower_lsp::lsp_types::GotoDefinitionParams {
                text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
                    text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri },
                    position: tower_lsp::lsp_types::Position::new(line, col),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            };
            match service.inner().goto_definition(params).await.expect("no error") {
                Some(tower_lsp::lsp_types::GotoDefinitionResponse::Array(v)) if !v.is_empty() => {
                    // Canonicalised on both sides: on Windows `demo` comes back with the
                    // \\?\\ prefix and the URI round-trip does not.
                    Some(v[0].uri.to_file_path().unwrap().canonicalize().unwrap())
                }
                _ => None,
            }
        }

        let app = demo.join("templates/app.conf.j2");
        for (needle, want) in [
            ("\"base.conf.j2\"", "templates/base.conf.j2"),
            ("\"macros.j2\" as m", "templates/macros.j2"),
            ("\"partials/header.j2\"", "templates/partials/header.j2"),
            ("\"optional.conf.j2\"", "templates/optional.conf.j2"),
        ] {
            assert_eq!(
                jump(&service, &app, needle).await,
                Some(demo.join(want)),
                "jumping from {needle}"
            );
        }

        // `common.j2` is deliberately NOT asserted here: it has three call sites and three
        // answers, which is `one_include_rendered_three_ways_offers_all_three_files`.

        // Silence where it is owed: a dynamic target names nothing, so there is nothing to
        // jump to, and a literal that resolves to no file does not invent one.
        let optional = demo.join("templates/optional.conf.j2");
        assert_eq!(jump(&service, &optional, "tuning_file").await, None);
        assert_eq!(jump(&service, &optional, "\"partials/absent.j2\"").await, None);
        // The control, on the same file: a target that does exist still jumps, so the two
        // `None`s above are about those targets and not about this file being unreadable.
        assert_eq!(
            jump(&service, &optional, "\"partials/header.j2\"").await,
            Some(demo.join("templates/partials/header.j2")),
        );
    }

    /// The missing-include warning: exactly one demo template has an unreachable target, and
    /// the two silences beside it on the same file are what make the rule safe to ship.
    ///
    /// Held back until the call sites were visible, and this is why: the verdict is "no
    /// candidate on the search path of any task that renders this file", which cannot be
    /// asked from the template alone.
    #[test]
    fn the_demo_flags_the_unreachable_include_and_nothing_else() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "j2") {
                    out.push(p);
                }
            }
        }
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let mut paths = Vec::new();
        walk(&demo, &mut paths);
        let grammars =
            ansible_core::resolve::template_grammars(&demo, &ansible_core::fs::StdFs);
        paths.sort();
        let mut flagged: Vec<(String, String)> = Vec::new();
        let mut with_sites = 0;
        for path in &paths {
            let text = std::fs::read_to_string(path).expect("demo template");
            let ctx = ansible_core::workspace::FileContext::discover(path);
            let sites = super::Backend::render_sites_for(path, Some(&demo), &ctx);
            if !sites.is_empty() {
                with_sites += 1;
            }
            let (d, is_root) = grammar_of(&grammars, path);
            for diag in
                super::State::missing_include_diagnostics(&text, path, &ctx, &sites, &d, is_root)
            {
                flagged.push((
                    path.file_name().unwrap().to_string_lossy().to_string(),
                    diag.message,
                ));
            }
        }
        assert_eq!(flagged.len(), 1, "{flagged:?}");
        assert_eq!(flagged[0].0, "broken_include.conf.j2");
        assert!(flagged[0].1.contains("partials/nowhere.j2"), "{}", flagged[0].1);
        // The control on the same file: `ignore missing` names the SAME absent target and is
        // silent, and the target that exists is silent. One warning, not three.
        let broken = demo.join("templates/broken_include.conf.j2");
        let text = std::fs::read_to_string(&broken).unwrap();
        // Counted on the tag, not the bare name: the fixture's comments quote the name too.
        assert_eq!(
            text.matches("{% include \"partials/nowhere.j2\"").count(),
            2,
            "the fixture must still have BOTH spellings, or the `ignore missing` silence is              not being tested"
        );

        // The other control: several demo templates DO have call sites, so "one warning"
        // is not "the walk found no call sites anywhere and reported nothing".
        assert!(with_sites >= 4, "only {with_sites} demo templates have a call site");
    }

    /// The render-site cache: hit, and the two invalidation rules that make it safe.
    ///
    /// The cache exists because `render_sites` reads the whole workspace and the server asks
    /// per keystroke. Its safety rests entirely on the invalidation, so both halves are
    /// asserted — a YAML change clears it, and a `.j2` change does not, which is the whole
    /// reason typing in a template stays fast.
    #[tokio::test]
    async fn the_render_site_cache_survives_j2_edits_and_not_yaml_ones() {
        use tower_lsp::LanguageServer;
        let root = ansible_core::testing::project(
            "render-site-cache",
            "[defaults]
",
            &[
                ("templates/t.j2", "{% include 'p.j2' %}
"),
                ("templates/p.j2", "leaf
"),
                ("play.yml", "- hosts: all
  tasks:
    - template: {src: t.j2, dest: /x}
"),
            ],
        );
        let state = scan_state(&root);
        let service = lsp_service(state.clone());
        let tpl = root.join("templates/t.j2");
        let ctx = ansible_core::workspace::FileContext::discover(&tpl);

        let n = |s: &super::State| s.render_sites.lock().unwrap().len();
        assert_eq!(n(&state), 0, "starts empty");

        let sites = super::Backend::render_sites_cached(&state, &tpl, Some(&root), &ctx);
        assert_eq!(sites.len(), 1, "play.yml renders it");
        assert_eq!(n(&state), 1, "and the answer was kept");

        // A `.j2` edit must NOT clear it — this is the per-keystroke case the cache is for.
        async fn edit(
            service: &tower_lsp::LspService<super::Backend>,
            path: std::path::PathBuf,
            text: &str,
        ) {
            use tower_lsp::LanguageServer;
            service
                .inner()
                .did_change(tower_lsp::lsp_types::DidChangeTextDocumentParams {
                    text_document: tower_lsp::lsp_types::VersionedTextDocumentIdentifier {
                        uri: tower_lsp::lsp_types::Url::from_file_path(path).unwrap(),
                        version: 2,
                    },
                    content_changes: vec![
                        tower_lsp::lsp_types::TextDocumentContentChangeEvent {
                            range: None,
                            range_length: None,
                            text: text.to_string(),
                        },
                    ],
                })
                .await;
        }
        // Asserted by identity, not by map size. `did_change` ends in `publish_diagnostics`,
        // which for a `.j2` asks for the render sites again and refills the cache — so the
        // size is 1 either way and a size assertion here could not fail. The `Arc` is the same
        // allocation only if the entry was never dropped.
        let before = super::Backend::render_sites_cached(&state, &tpl, Some(&root), &ctx);
        edit(&service, root.join("templates/t.j2"), "{% include 'p.j2' %}x
").await;
        let after = super::Backend::render_sites_cached(&state, &tpl, Some(&root), &ctx);
        assert!(
            std::sync::Arc::ptr_eq(&before, &after),
            "a .j2 edit recomputed the render sites — the per-keystroke case is not cached"
        );

        // A YAML edit must clear it: that file may have gained or lost a `template:` task.
        edit(&service, root.join("play.yml"), "- hosts: all
  tasks: []
").await;
        assert_eq!(n(&state), 0, "a YAML edit must clear the cache");
    }

    /// The whole tree's grammars in one pass, then a lookup — never per file. Asking
    /// `effective_delimiters` once per template re-walks the workspace once per template.
    fn grammar_of(
        map: &std::collections::HashMap<std::path::PathBuf, ansible_core::resolve::TemplateGrammar>,
        template: &std::path::Path,
    ) -> (ansible_core::jinja::Delimiters, bool) {
        let key = template.canonicalize().unwrap_or_else(|_| template.to_path_buf());
        match map.get(&key) {
            Some(g) => (g.delimiters.clone(), g.root),
            None => (ansible_core::jinja::Delimiters::default(), true),
        }
    }

    /// The candidates box. `demo/templates/common.j2` holds one `{% include "shared.j2" %}`
    /// and is rendered from three places; ansible-core 2.21.2 gives a different file in each,
    /// measured. So the answer is three locations, not one, and the editor shows a picker.
    #[tokio::test]
    async fn one_include_rendered_three_ways_offers_all_three_files() {
        use tower_lsp::LanguageServer;
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let service = lsp_service(scan_state(&demo));
        let path = demo.join("templates/common.j2");
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = super::Document::new(text.clone());
        let byte = text.find("\"shared.j2\"").expect("the include is still there") + 2;
        let (line, col) = doc.byte_to_lsp(byte);
        let uri = tower_lsp::lsp_types::Url::from_file_path(&path).unwrap();
        service
            .inner()
            .did_open(tower_lsp::lsp_types::DidOpenTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "jinja".into(),
                    version: 1,
                    text,
                },
            })
            .await;
        let got = service
            .inner()
            .goto_definition(tower_lsp::lsp_types::GotoDefinitionParams {
                text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
                    text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri },
                    position: tower_lsp::lsp_types::Position::new(line, col),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await
            .expect("no error");
        let mut files: Vec<String> = match got {
            Some(tower_lsp::lsp_types::GotoDefinitionResponse::Array(v)) => v
                .into_iter()
                .map(|l| {
                    let p = l.uri.to_file_path().unwrap().canonicalize().unwrap();
                    p.strip_prefix(&demo).unwrap().to_string_lossy().replace('\\', "/")
                })
                .collect(),
            other => panic!("expected an array, got {other:?}"),
        };
        files.sort();
        assert_eq!(
            files,
            [
                "roles/edge-cache/templates/shared.j2",
                "roles/edge-proxy/templates/shared.j2",
                "templates/shared.j2",
            ],
            "one include line, three call sites, three answers"
        );
    }

    /// T-040's nine measured rows, through the surface a user sees. Every one is a template
    /// jinja2 3.1.6 refuses, so every one must come back as exactly one `template-syntax`
    /// ERROR — and the repaired form of each must come back clean, which is the control that
    /// stops "refuse everything" from passing.
    #[test]
    fn a_template_that_will_not_render_is_one_error_and_its_repair_is_none() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let one = |src: &str| {
            super::State::template_diagnostics_at(src, &[], &ansible_core::jinja::Delimiters::default(), true)
        };
        for (broken, repaired) in [
            ("{% for x in xs %}{{ x }}", "{% for x in xs %}{{ x }}{% endfor %}"),
            ("{% if a %}x{% endfor %}", "{% if a %}x{% endif %}"),
            ("{% forr x in xs %}{% endforr %}", "{% for x in xs %}{% endfor %}"),
            ("{% include 'a.j2' %}{% endif %}", "{% include 'a.j2' %}"),
            ("{% macro m(a,) %}{% endmacro %}", "{% macro m(a) %}{% endmacro %}"),
            ("{% set x = %}", "{% set x = 1 %}"),
            ("{% for x in %}{% endfor %}", "{% for x in xs %}{% endfor %}"),
            ("{% raw %}{% include 'x.j2' %}", "{% raw %}{% include 'x.j2' %}{% endraw %}"),
            ("{{ x }", "{{ x }}"),
        ] {
            let got = one(broken);
            assert_eq!(got.len(), 1, "{broken:?} produced {got:?}");
            assert_eq!(got[0].severity, Some(DiagnosticSeverity::ERROR));
            assert!(matches!(&got[0].code, Some(NumberOrString::String(s)) if s == "template-syntax"));
            assert!(one(repaired).is_empty(), "{repaired:?} was flagged: {:?}", one(repaired));
        }
    }

    /// The `DEFAULT_JINJA2_EXTENSIONS` gate T-040 requires. An extension registers tags, and
    /// `Extension.preprocess` can rewrite the source before it is lexed at all, so with one
    /// configured no refusal of ours is safe to report.
    ///
    /// Live-measured on ansible-core 2.21.2, which is what makes this a gate and not a
    /// precaution: `{% for i in [1,2,3] %}{% if i == 2 %}{% break %}{% endif %}{{ i }}{% endfor %}`
    /// is `Encountered unknown tag 'break'` by default and renders `1` under
    /// `ANSIBLE_JINJA2_EXTENSIONS=jinja2.ext.loopcontrols` (and under the same value written
    /// as `[defaults] jinja2_extensions`).
    #[test]
    fn a_configured_jinja_extension_silences_the_template_diagnostic() {
        let src = "{% for i in [1,2,3] %}{% if i == 2 %}{% break %}{% endif %}{{ i }}{% endfor %}";
        // The control: with no extension it is reported, and reported as the unknown tag.
        let d = ansible_core::jinja::Delimiters::default();
        let bare = super::State::template_diagnostics_at(src, &[], &d, true);
        assert_eq!(bare.len(), 1, "{bare:?}");
        assert!(bare[0].message.contains("break"), "{}", bare[0].message);
        let loaded = ["jinja2.ext.loopcontrols".to_string()];
        assert!(super::State::template_diagnostics_at(src, &loaded, &d, true).is_empty());
        // Not only the unknown-tag class: `preprocess` reaches everything, so the gate is
        // the whole file.
        assert_eq!(super::State::template_diagnostics_at("{{ x }", &[], &d, true).len(), 1);
        assert!(super::State::template_diagnostics_at("{{ x }", &loaded, &d, true).is_empty());
    }

    /// Rule 4: exactly the three demo templates labelled BAD get a `template-syntax` ERROR,
    /// and every other demo template is labelled as one that renders. Both halves asserted, so
    /// the rule cannot start firing elsewhere unnoticed.
    ///
    /// `partials/inherited.j2` is the third and the interesting one: nothing about its bytes
    /// is wrong. It is unparseable only in the grammar its includer imposes, so it is flagged
    /// only because the include graph is read — with the graph ignored it comes back clean and
    /// ansible fails the render.
    ///
    /// `overridden.conf.j2` is the row that earns the `#jinja2:` reader: it contains a
    /// `{% notatag %}` that is ordinary text under its own delimiters and an unknown tag under
    /// the default ones. Stop reading the header and it joins the flagged list — a red squiggle
    /// on a template ansible renders without complaint.
    #[test]
    fn the_demo_flags_the_three_bad_templates_and_no_other() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "j2") {
                    out.push(p);
                }
            }
        }
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let mut paths = Vec::new();
        walk(&demo, &mut paths);
        let grammars =
            ansible_core::resolve::template_grammars(&demo, &ansible_core::fs::StdFs);
        assert!(paths.len() > 5, "the demo walk found no templates");
        let mut flagged = Vec::new();
        let mut default_flagged = Vec::new();
        for path in &paths {
            let text = std::fs::read_to_string(path).expect("demo template");
            // Through the call-site link, as the server does: the delimiters a template is
            // read with can live in the task that renders it.
            let ctx = ansible_core::workspace::FileContext::discover(path);
            let sites = super::Backend::render_sites_for(path, Some(&demo), &ctx);
            let (d, is_root) = grammar_of(&grammars, path);
            let plain = ansible_core::jinja::Delimiters::default();
            if !super::State::template_diagnostics_at(&text, &[], &plain, true).is_empty() {
                default_flagged
                    .push(path.file_name().unwrap().to_string_lossy().to_string());
            }
            let diags = super::State::template_diagnostics_at(&text, &[], &d, is_root);
            if !diags.is_empty() {
                flagged.push((
                    path.file_name().unwrap().to_string_lossy().to_string(),
                    diags[0].message.clone(),
                ));
            }
        }
        flagged.sort();
        let names: Vec<&str> = flagged.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["bad_header.conf.j2", "broken.conf.j2", "inherited.j2"],
            "{flagged:?}"
        );
        assert!(flagged[0].1.contains("nosuchkey"), "{}", flagged[0].1);
        assert!(flagged[1].1.contains("forr"), "{}", flagged[1].1);
        assert!(flagged[2].1.contains("notatag"), "{}", flagged[2].1);
        // The control for the third: read as its own root — which is what we did before the
        // include graph was walked — it is clean, and ansible still fails the render.
        let inherited = demo.join("templates/partials/inherited.j2");
        let text = std::fs::read_to_string(&inherited).unwrap();
        assert!(
            super::State::template_diagnostics_at(
                &text,
                &[],
                &ansible_core::jinja::Delimiters::default(),
                true
            )
            .is_empty(),
            "the fixture stopped being fine on its own bytes"
        );

        // The control, and the reason the link exists: read with the DEFAULT delimiters,
        // `module_delims.j2` is a false positive — its `{% notatag %}` is text only because
        // the task that renders it moved the block delimiters. Ansible renders it `ok`.
        default_flagged.sort();
        assert!(
            default_flagged.contains(&"module_delims.j2".to_string()),
            "the fixture stopped exercising the call-site delimiters: {default_flagged:?}"
        );
        // Every `.j2` in the demo is routed as a template, never as broken YAML.
        assert!(paths.iter().all(|p| super::Backend::is_template_file(p)));
        assert!(!super::Backend::is_template_file(&demo.join("playbook.yml")));
    }

    /// T-100 per T-010: the rule fires on code ansible-core accepts, so a role that really
    /// does take a keyword-named param must be able to say so.
    #[test]
    fn noqa_suppresses_role_param_not_keyword() {
        use tower_lsp::lsp_types::NumberOrString;
        let path = std::path::Path::new("../../demo").canonicalize().unwrap().join("probe.yml");
        let flagged = |text: &str| {
            let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
            super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter(|d| {
                    matches!(&d.code, Some(NumberOrString::String(s))
                        if s == ansible_core::attributes::ROLE_PARAM_RULE_ID)
                })
                .count()
        };
        let noisy = "- hosts: web\n  roles:\n    - role: r\n      tasks_from: x.yml\n";
        assert_eq!(flagged(noisy), 1);
        let silenced = "- hosts: web\n  roles:\n    - role: r\n      \
                        tasks_from: x.yml # noqa: role-param-not-keyword\n";
        assert_eq!(flagged(silenced), 0);
        // The replication rules keep their own id — silencing one must not silence both.
        let wrong_id = "- hosts: web\n  roles:\n    - role: r\n      \
                        tasks_from: x.yml # noqa: invalid-attribute\n";
        assert_eq!(flagged(wrong_id), 1);
    }

    /// T-110: `# noqa: invalid-placement` on the offending line silences the rule.
    #[test]
    fn noqa_suppresses_invalid_placement() {
        use tower_lsp::lsp_types::NumberOrString;
        let path = std::path::Path::new("../../demo").canonicalize().unwrap().join("probe.yml");
        let flagged = |text: &str| {
            let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
            super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter(|d| {
                    matches!(&d.code, Some(NumberOrString::String(s)) if s == "invalid-placement")
                })
                .count()
        };
        let noisy = "- hosts: web\n  user: alice\n  remote_user: bob\n  tasks: []\n";
        assert_eq!(flagged(noisy), 1);
        let silenced =
            "- hosts: web\n  user: alice # noqa: invalid-placement\n  remote_user: bob\n  tasks: []\n";
        assert_eq!(flagged(silenced), 0);
    }

    /// T-103's fixture box, both directions: every line annotated BAD/WARN carries exactly
    /// one `static-template` diagnostic of the annotated severity, every GOOD line carries
    /// none, and no unannotated line fires. The SILENCED row is covered by the noqa test
    /// below and by not being annotated here.
    #[test]
    fn the_static_templates_demo_matches_its_annotations_exactly() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path =
            std::path::Path::new("../../demo/static_templates.yml").canonicalize().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text.clone(), &path).unwrap();
        let ours: Vec<(u32, DiagnosticSeverity)> = super::Backend::diagnostics_of(&a)
            .iter()
            .filter(|d| {
                matches!(&d.code, Some(NumberOrString::String(s)) if s == "static-template")
            })
            .map(|d| (d.range.start.line, d.severity.unwrap()))
            .collect();

        let src: Vec<&str> = text.lines().collect();
        let mut expected: Vec<(u32, DiagnosticSeverity)> = Vec::new();
        for (i, line) in src.iter().enumerate() {
            let severity = if line.contains("# BAD") {
                DiagnosticSeverity::ERROR
            } else if line.contains("# WARN") {
                DiagnosticSeverity::WARNING
            } else {
                continue;
            };
            // An annotation on its own comment line heads the next non-comment line.
            let target = if line.trim_start().starts_with('#') {
                i + 1
                    + src[i + 1..]
                        .iter()
                        .position(|l| !l.trim_start().starts_with('#'))
                        .unwrap()
            } else {
                i
            };
            expected.push((target as u32, severity));
        }
        let mut got = ours.clone();
        got.sort_unstable();
        expected.sort_unstable();
        assert_eq!(got, expected, "diagnostics and annotations disagree");
        // The fixture exercises both tiers, so a severity regression cannot pass.
        assert!(expected.iter().any(|(_, s)| *s == DiagnosticSeverity::ERROR));
        assert!(expected.iter().any(|(_, s)| *s == DiagnosticSeverity::WARNING));
    }

    /// T-103's false-positive gate: templates in fields that DO template — `notify:`,
    /// `loop:`, `vars:` values and friends all over the demo tree — must never fire this
    /// rule, and neither may data files with keyword-shaped keys (`requirements.yml`'s
    /// top-level `collections:`).
    #[test]
    fn every_other_demo_file_is_free_of_static_template_diagnostics() {
        use tower_lsp::lsp_types::NumberOrString;
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let files = ansible_core::workspace::yaml_files(&demo);
        assert!(files.len() > 10, "the demo walk found the demo");
        for path in files {
            if path.file_name().is_some_and(|n| n == "static_templates.yml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Some(a) = super::Backend::analyze_text(text, &path) else { continue };
            let bad: Vec<String> = super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter(|d| {
                    matches!(&d.code, Some(NumberOrString::String(s)) if s == "static-template")
                })
                .map(|d| d.message)
                .collect();
            assert!(bad.is_empty(), "{}: {bad:?}", path.display());
        }
    }

    /// T-168's fixture box, both directions: every BAD line carries exactly one
    /// `complex-key` error, every GOOD line carries none, and no unannotated line fires
    /// — the SILENCED row is pinned by staying out of both sets.
    #[test]
    fn the_complex_keys_demo_matches_its_annotations_exactly() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path = std::path::Path::new("../../demo/complex_keys.yml").canonicalize().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text.clone(), &path).unwrap();
        let mut got: Vec<u32> = super::Backend::diagnostics_of(&a)
            .iter()
            .filter(|d| {
                matches!(&d.code, Some(NumberOrString::String(s)) if s == "complex-key")
                    && d.severity == Some(DiagnosticSeverity::ERROR)
            })
            .map(|d| d.range.start.line)
            .collect();
        let mut expected: Vec<u32> = text
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains("# BAD"))
            .map(|(i, _)| i as u32)
            .collect();
        got.sort_unstable();
        expected.sort_unstable();
        assert!(expected.len() >= 3, "the fixture lost its BAD rows");
        assert_eq!(got, expected, "diagnostics and annotations disagree");
    }

    /// T-168's false-positive gate: quoted template keys and ordinary mappings all over
    /// the demo tree must never fire the rule.
    #[test]
    fn every_other_demo_file_is_free_of_complex_key_diagnostics() {
        use tower_lsp::lsp_types::NumberOrString;
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let files = ansible_core::workspace::yaml_files(&demo);
        assert!(files.len() > 10, "the demo walk found the demo");
        for path in files {
            if path.file_name().is_some_and(|n| n == "complex_keys.yml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Some(a) = super::Backend::analyze_text(text, &path) else { continue };
            let bad: Vec<String> = super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter(|d| {
                    matches!(&d.code, Some(NumberOrString::String(s)) if s == "complex-key")
                })
                .map(|d| d.message)
                .collect();
            assert!(bad.is_empty(), "{}: {bad:?}", path.display());
        }
    }

    /// T-168: `# noqa: complex-key` on the offending line silences the rule, and a
    /// different rule's id does not.
    #[test]
    fn noqa_suppresses_complex_key() {
        use tower_lsp::lsp_types::NumberOrString;
        let path = std::path::Path::new("../../demo").canonicalize().unwrap().join("probe.yml");
        let flagged = |text: &str| {
            let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
            super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter(|d| {
                    matches!(&d.code, Some(NumberOrString::String(s)) if s == "complex-key")
                })
                .count()
        };
        let noisy = "- hosts: web\n  tasks:\n    - set_fact:\n        {{ v }}: true\n";
        assert_eq!(flagged(noisy), 1);
        let silenced =
            "- hosts: web\n  tasks:\n    - set_fact:\n        {{ v }}: true # noqa: complex-key\n";
        assert_eq!(flagged(silenced), 0);
        let wrong_id =
            "- hosts: web\n  tasks:\n    - set_fact:\n        {{ v }}: true # noqa: static-template\n";
        assert_eq!(flagged(wrong_id), 1);
    }

    /// T-103: `# noqa: static-template` on the offending line silences the rule, and a
    /// different rule's id does not.
    #[test]
    fn noqa_suppresses_static_template() {
        use tower_lsp::lsp_types::NumberOrString;
        let path = std::path::Path::new("../../demo").canonicalize().unwrap().join("probe.yml");
        let flagged = |text: &str| {
            let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
            super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter(|d| {
                    matches!(&d.code, Some(NumberOrString::String(s)) if s == "static-template")
                })
                .count()
        };
        let noisy = "- hosts: web\n  tasks:\n    - command: whoami\n      register: \"{{ v }}\"\n";
        assert_eq!(flagged(noisy), 1);
        let silenced = "- hosts: web\n  tasks:\n    - command: whoami\n      \
                        register: \"{{ v }}\" # noqa: static-template\n";
        assert_eq!(flagged(silenced), 0);
        let wrong_id = "- hosts: web\n  tasks:\n    - command: whoami\n      \
                        register: \"{{ v }}\" # noqa: invalid-attribute\n";
        assert_eq!(flagged(wrong_id), 1);
    }

    /// A misplaced `import_playbook:` gets exactly one diagnostic. Its target is never opened
    /// by ansible, so neither the file's absence nor its contents is a fact about this mistake
    /// — both would be a second complaint about a line that has one thing wrong with it.
    #[test]
    fn a_misplaced_import_playbook_is_reported_once() {
        use tower_lsp::lsp_types::NumberOrString;
        let path = std::path::Path::new("../../demo").canonicalize().unwrap().join("probe.yml");
        let ids = |src: &str| {
            let a = super::Backend::analyze_text(src.to_string(), &path).unwrap();
            let mut v: Vec<String> = super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter_map(|d| match d.code {
                    Some(NumberOrString::String(s)) => Some(s),
                    _ => None,
                })
                .collect();
            v.sort();
            v
        };
        // The target does not exist: `missing-file` would be describing a lookup ansible never
        // performs.
        assert_eq!(
            ids("- hosts: localhost\n  tasks:\n    - import_playbook: nope.yml\n"),
            ["misplaced-import-playbook"]
        );
        // The target exists and is an empty playbook: row 8's import rule must stay out of it
        // too, for the same reason.
        assert_eq!(
            ids("- hosts: localhost\n  tasks:\n    - import_playbook: plays/empty_playbook.yml\n"),
            ["misplaced-import-playbook"]
        );
        // The control: at playbook level, both rules are exactly right to fire.
        assert_eq!(ids("- import_playbook: nope.yml\n"), ["missing-file"]);
        assert_eq!(ids("- import_playbook: plays/empty_playbook.yml\n"), ["empty-playbook"]);
    }

    /// Row 29. Its own id, because someone using a reserved name on purpose wants to silence
    /// this and not every replication sharing the line — and because the message is not one:
    /// upstream randomises the order of the names, so ours sorts them.
    #[test]
    fn the_reserved_tag_rule_is_its_own_and_sorts_its_names() {
        use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
        let path = std::path::Path::new("../../demo/placement.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let got: Vec<_> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(
                |d| matches!(&d.code, Some(NumberOrString::String(s)) if s == "reserved-tag-name"),
            )
            .collect();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(
            got[0].message,
            "Found reserved tagnames in tags: ['all'], we do not recommend doing this as it \
             might give unexpected results"
        );
    }

    /// Rows 5 and 23 suppress on their own ids. Two ids rather than one because the author's
    /// answer differs: an empty target is often a file not written yet, while the wrong shape
    /// is always a mistake.
    #[test]
    fn noqa_suppresses_the_include_target_rules_separately() {
        use tower_lsp::lsp_types::NumberOrString;
        let path = std::path::Path::new("../../demo").canonicalize().unwrap().join("probe.yml");
        let codes = |text: &str| {
            let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
            let mut ids: Vec<String> = super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter_map(|d| match d.code {
                    Some(NumberOrString::String(s))
                        if s == "empty-task-file" || s == "invalid-task-file" =>
                    {
                        Some(s)
                    }
                    _ => None,
                })
                .collect();
            ids.sort();
            ids
        };
        let noisy = "- hosts: web\n  tasks:\n    - import_tasks: tasks/empty_target.yml\n      \
                     \n    - import_tasks: tasks/mapping_target.yml\n";
        assert_eq!(codes(noisy), ["empty-task-file", "invalid-task-file"]);
        // Silencing one leaves the other speaking.
        let half = "- hosts: web\n  tasks:\n    - import_tasks: tasks/empty_target.yml # noqa: \
                    empty-task-file\n    - import_tasks: tasks/mapping_target.yml\n";
        assert_eq!(codes(half), ["invalid-task-file"]);
    }

    /// T-110: the divergent rule has its own id, so it suppresses on its own — and the
    /// replication id must NOT silence it, or the two would be one rule wearing two names.
    #[test]
    fn noqa_suppresses_shadowed_loop_independently() {
        use tower_lsp::lsp_types::NumberOrString;
        let path = std::path::Path::new("../../demo").canonicalize().unwrap().join("probe.yml");
        let codes = |text: &str| {
            let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
            super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter_map(|d| match d.code {
                    Some(NumberOrString::String(s)) if s == "shadowed-loop" => Some(s),
                    _ => None,
                })
                .count()
        };
        let noisy = "- hosts: web\n  tasks:\n    - debug:\n      with_items: [a]\n      loop: [1]\n";
        assert_eq!(codes(noisy), 1);

        // The noqa goes on the line the diagnostic is anchored to: the dead `with_*`.
        let silenced = "- hosts: web\n  tasks:\n    - debug:\n      \
                        with_items: [a] # noqa: shadowed-loop\n      loop: [1]\n";
        assert_eq!(codes(silenced), 0);

        // The other id must not reach it.
        let wrong_id = "- hosts: web\n  tasks:\n    - debug:\n      \
                        with_items: [a] # noqa: invalid-placement\n      loop: [1]\n";
        assert_eq!(codes(wrong_id), 1, "invalid-placement must not silence shadowed-loop");
    }

    /// T-155: same contract for the dead-`loop_control` warning — its own id, and the
    /// replication id does not reach it.
    #[test]
    fn noqa_suppresses_dead_loop_control_independently() {
        use tower_lsp::lsp_types::NumberOrString;
        let path = std::path::Path::new("../../demo").canonicalize().unwrap().join("probe.yml");
        let count = |text: &str| {
            let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
            super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter(|d| {
                    matches!(&d.code, Some(NumberOrString::String(s)) if s == "dead-loop-control")
                })
                .count()
        };
        let noisy = "- hosts: web\n  tasks:\n    - debug:\n      loop_control: {loop_var: it}\n";
        assert_eq!(count(noisy), 1);
        let silenced = "- hosts: web\n  tasks:\n    - debug:\n      \
                        loop_control: {loop_var: it} # noqa: dead-loop-control\n";
        assert_eq!(count(silenced), 0);
        let wrong_id = "- hosts: web\n  tasks:\n    - debug:\n      \
                        loop_control: {loop_var: it} # noqa: invalid-placement\n";
        assert_eq!(count(wrong_id), 1, "invalid-placement must not silence dead-loop-control");
    }

    /// T-147, the diagnostics consumer. `attributes.rs` owns the message and the key set;
    /// what this pins is the *routing* — which files reach the rule at all. A top-level
    /// mapping is `Ast::Other`, so nothing reaches it by accident, and every row below
    /// differs from the first only in where the file sits or what it is called.
    ///
    /// The three silent rows are the control, and they are what a path check that is merely
    /// "ends with meta/main.yml" gets wrong: `meta/argument_specs.yml` is a sibling under a
    /// different schema (T-149), and a `meta/main.yml` with no role around it is not role
    /// metadata at all — it is a vars file with an unlucky name.
    #[test]
    fn only_a_role_s_own_meta_main_is_validated_as_role_metadata() {
        use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString};
        let d = std::env::temp_dir().join("ansible-lsp-t147-routing");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("roles/r/meta")).unwrap();
        std::fs::create_dir_all(d.join("roles/r/tasks")).unwrap();
        std::fs::create_dir_all(d.join("plain")).unwrap();

        let bad = "dependencies: []\nwhen: true\n";
        let flagged = |rel: &str, text: &str| -> Vec<Diagnostic> {
            let path = d.join(rel);
            std::fs::write(&path, text).unwrap();
            let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
            super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter(|x| {
                    matches!(&x.code, Some(NumberOrString::String(s)) if s == "invalid-attribute")
                })
                .collect()
        };

        let got = flagged("roles/r/meta/main.yml", bad);
        assert_eq!(got.len(), 1, "the role's own meta/main.yml is RoleMetadata: {got:?}");
        assert_eq!(got[0].message, "'when' is not a valid attribute for a RoleMetadata");
        assert_eq!(got[0].severity, Some(DiagnosticSeverity::ERROR), "fatal at load, never a warning");
        // The span is the key, not the line or the value — the squiggle sits under `when`.
        assert_eq!(got[0].range.start.line, 1);
        assert_eq!((got[0].range.start.character, got[0].range.end.character), (0, 4));

        // `_load_role_yaml` hard-codes `.yml .yaml .json`, in that order, so the `.yaml`
        // spelling is the same file to Ansible and must be to us.
        assert_eq!(
            flagged("roles/r/meta/main.yaml", bad).len(),
            1,
            "meta/main.yaml is role metadata too"
        );
        assert!(
            flagged("roles/r/meta/argument_specs.yml", bad).is_empty(),
            "argument_specs.yml is a different schema (T-149), not RoleMetadata"
        );
        assert!(
            flagged("plain/main.yml", bad).is_empty(),
            "a main.yml outside a role's meta/ is not role metadata"
        );
        assert!(
            flagged("roles/r/tasks/main.yml", "- debug: {msg: x}\n").is_empty(),
            "the control that must come out different if the rule fired on everything"
        );
        // The row that made this test able to fail. A role's `vars/main.yml` is also a
        // top-level mapping under `<role>/`, and its keys are *variable names* — `when:` and
        // `become:` are ordinary variables there. A predicate that checks the filename but
        // forgets the directory turns every one of them into a false ERROR on valid Ansible,
        // and the tasks/ row above cannot catch that: a task file is a sequence, so the
        // non-mapping guard hides the bug.
        for dir in ["vars", "defaults"] {
            std::fs::create_dir_all(d.join("roles/r").join(dir)).unwrap();
            assert!(
                flagged(&format!("roles/r/{dir}/main.yml"), bad).is_empty(),
                "keys in a role's {dir}/main.yml are variable names, not RoleMetadata keys"
            );
        }

        let silenced = "dependencies: []\nwhen: true # noqa: invalid-attribute\n";
        assert!(
            flagged("roles/r/meta/main.yml", silenced).is_empty(),
            "# noqa: invalid-attribute suppresses it like every other arm"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// T-088: `# noqa: invalid-attribute` on the offending line silences the rule.
    #[test]
    fn noqa_suppresses_invalid_attribute() {
        use tower_lsp::lsp_types::NumberOrString;
        let path = std::path::Path::new("../../demo").canonicalize().unwrap().join("probe.yml");
        let flagged = |text: &str| {
            let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
            super::Backend::diagnostics_of(&a)
                .into_iter()
                .filter(|d| {
                    matches!(&d.code, Some(NumberOrString::String(s)) if s == "invalid-attribute")
                })
                .count()
        };
        let bad = "- hosts: web\n  vars_file: x.yml\n  tasks:\n    - debug:\n";
        assert_eq!(flagged(bad), 1);
        let silenced = "- hosts: web\n  vars_file: x.yml # noqa: invalid-attribute\n  tasks:\n    - debug:\n";
        assert_eq!(flagged(silenced), 0);
    }

    /// T-016: the messages must describe what Ansible actually does with a missing
    /// vars_files entry — silent skip — and never claim the play would fail.
    #[test]
    fn vars_files_messages_state_runtime_silence_not_error() {
        use tower_lsp::lsp_types::NumberOrString;
        let path = std::path::Path::new("../../demo/vars_files_demo.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        for d in super::Backend::diagnostics_of(&a) {
            if !matches!(&d.code, Some(NumberOrString::String(s)) if s == "missing-file") {
                continue;
            }
            assert!(
                d.message.contains("silently skips"),
                "must state the runtime behavior: {}",
                d.message
            );
            assert!(
                !d.message.to_lowercase().contains("error") && !d.message.contains("fail"),
                "must not claim Ansible errors: {}",
                d.message
            );
        }
    }

    /// T-016: a resolved first-match group must not paint a link over the whole nested
    /// list — the winning alternative carries its own.
    #[test]
    fn vars_files_group_anchor_is_not_linked() {
        let path = std::path::Path::new("../../demo/vars_files_demo.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let group = a
            .refs
            .iter()
            .find(|(r, res)| {
                r.vars_files_group.is_some()
                    && res.status == ansible_core::resolve::Status::Resolved
            })
            .expect("a satisfied group in the demo");
        assert!(!super::linkable(&group.0, &group.1));
        let winner = a
            .refs
            .iter()
            .find(|(r, res)| {
                r.grouped
                    && r.value == "vars/shared.yml"
                    && res.status == ansible_core::resolve::Status::Resolved
            })
            .expect("the winning alternative");
        assert!(super::linkable(&winner.0, &winner.1));
    }

    /// T-016: `# noqa: missing-file` on the group's first line suppresses the group
    /// warning, same as any other missing-file site.
    #[test]
    fn noqa_suppresses_the_group_warning() {
        use tower_lsp::lsp_types::NumberOrString;
        let dir = std::env::temp_dir().join("ansible-lsp-t016-noqa");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("site.yml");
        let text = "- hosts: all\n  vars_files:\n    - - nope-a.yml  # noqa: missing-file\n      - nope-b.yml\n";
        std::fs::write(&path, text).unwrap();
        let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
        let missing = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(|d| matches!(&d.code, Some(NumberOrString::String(s)) if s == "missing-file"))
            .count();
        assert_eq!(missing, 0, "suppressed by the noqa on the group's first line");
    }

    /// T-095: the message must name the two sources that work, the provable harm, and the
    /// escape hatch — and the escape hatch must actually work, since the message now sends
    /// people to it. "I do pass `-e`" is a legitimate answer only the author can give.
    #[test]
    fn templated_import_message_names_the_fix_and_noqa_silences_it() {
        use tower_lsp::lsp_types::NumberOrString;
        let dir = std::env::temp_dir().join("ansible-lsp-t095");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ansible.cfg"), "[defaults]\n").unwrap();
        let path = dir.join("site.yml");
        let is_t095 = |d: &tower_lsp::lsp_types::Diagnostic| {
            matches!(&d.code, Some(NumberOrString::String(s)) if s == "templated-import")
        };

        let text = "- import_playbook: \"{{ env }}-setup.yml\"\n";
        std::fs::write(&path, text).unwrap();
        let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
        let d = super::Backend::diagnostics_of(&a)
            .into_iter()
            .find(is_t095)
            .expect("templated-import diagnostic");
        for want in ["-e", "vars:", "--syntax-check", "noqa: templated-import"] {
            assert!(d.message.contains(want), "message must name {want:?}: {}", d.message);
        }
        // The old wording claimed it could never resolve. With `-e` it resolves and runs.
        assert!(
            !d.message.contains("cannot resolve"),
            "the false claim must be gone: {}",
            d.message
        );

        let text = "- import_playbook: \"{{ env }}-setup.yml\"  # noqa: templated-import\n";
        std::fs::write(&path, text).unwrap();
        let a = super::Backend::analyze_text(text.to_string(), &path).unwrap();
        assert!(
            !super::Backend::diagnostics_of(&a).iter().any(is_t095),
            "the noqa the message advertises has to work"
        );
    }

    /// T-016: the second `vars_files` site in the repo's demo must stay warning-free.
    #[test]
    fn cross_file_vars_demo_vars_files_stays_silent() {
        let path = std::path::Path::new("../../demo/cross_file_vars.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        assert!(
            a.refs
                .iter()
                .filter(|(r, _)| r.kind == ansible_core::references::ReferenceKind::VarsFiles)
                .all(|(_, res)| res.status == ansible_core::resolve::Status::Resolved),
            "vars/shared.yml resolves"
        );
    }

    /// T-016 box: hovering a variable a vars_files file defines shows the vars_files
    /// provenance and the value.
    #[test]
    fn vars_files_variable_hover_shows_provenance() {
        let path = std::path::Path::new("../../demo/vars_files_demo.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = ansible_core::parse::Document::new(text.clone());
        let nodes = doc.parse().unwrap();
        let byte = text.find("{{ shared_endpoint }}").unwrap() + 3;
        let (md, _) = super::Backend::variable_hover_at(&doc, &nodes, byte, &path, &no_buffers(), &[], &no_cache(), None)
            .expect("hover expected");
        assert!(md.contains("vars_files"), "provenance label in: {md}");
        assert!(md.contains("vars/shared.yml"), "defining file in: {md}");
        assert!(md.contains("https://api.internal:8443"), "value in: {md}");
    }

    /// T-177 on the demo, which is where its labels become claims (rule 4).
    ///
    /// The whole fixture was run against 2.21.2 before this was written. It reaches the
    /// second play clean — `pod_namespace`, `pod_label` and the two leaked aliases all
    /// printed — and then dies on `{{ name }}` with `'name' is undefined`, which is the
    /// BAD row earning its label. The other BAD row was measured separately: the created
    /// host's keys really are `['literal_beside_it', '{{ dyn }}']`, so `dyn` is undefined
    /// and the templated key defines nothing any expression can reach.
    #[test]
    fn the_add_host_demo_flags_the_consumed_parameter_and_nothing_it_defines() {
        use tower_lsp::lsp_types::NumberOrString;
        let path = std::path::Path::new("../../demo/add_host_vars.yml").canonicalize().unwrap();
        let text = std::fs::read_to_string(&path).expect("demo fixture");
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let undefined: Vec<String> =
            super::Backend::variable_coverage_diagnostics(&a, &path, &a.nodes, &[], &no_cache())
                .into_iter()
                .filter(|d| matches!(&d.code, Some(NumberOrString::String(s)) if s == "var-undefined"))
                .map(|d| d.message)
                .collect();

        // Exactly the two BAD rows. Asserting the whole set, not just that the GOOD rows
        // are quiet: a rule that fired on everything would satisfy "pod_namespace is not
        // reported" only by accident.
        assert_eq!(undefined.len(), 2, "{undefined:?}");
        assert!(undefined.iter().any(|m| m.contains("`name`")), "{undefined:?}");
        assert!(undefined.iter().any(|m| m.contains("`dyn`")), "{undefined:?}");
        for quiet in ["pod_namespace", "pod_label", "`host`", "`group`"] {
            assert!(
                !undefined.iter().any(|m| m.contains(quiet)),
                "{quiet} is defined by the add_host and must not be reported: {undefined:?}"
            );
        }
    }

    /// T-177's editor consumers, asserted one per rule 3 rather than trusting that one
    /// index feeds both. The hover and the jump have contradicted each other before.
    #[test]
    fn an_add_host_variable_hovers_and_jumps_to_the_key_that_defined_it() {
        let path = std::path::Path::new("../../demo/add_host_vars.yml").canonicalize().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let uri = tower_lsp::lsp_types::Url::from_file_path(&path).unwrap();
        let doc = ansible_core::parse::Document::new(text.clone());
        let nodes = doc.parse().unwrap();

        let byte = text.find("{{ pod_namespace }}").unwrap() + 3;
        let (md, _) = super::Backend::variable_hover_at(&doc, &nodes, byte, &path, &no_buffers(), &[], &no_cache(), None)
            .expect("hover on an add_host-defined variable");
        assert!(md.contains("add_host"), "provenance label in: {md}");
        // The value, which is why the definition's span points at it rather than at the
        // key the way `set_fact` does.
        assert!(md.contains("my-namespace"), "value in: {md}");

        let (line, character) = doc.byte_to_lsp(byte);
        let locs = super::Backend::definition_at(
            &doc,
            &nodes,
            tower_lsp::lsp_types::Position { line, character },
            &uri,
            &path,
            &no_buffers(),
                        &[],
                            &no_cache(),
                            None,
            )
        .expect("jump from an add_host-defined variable");
        assert_eq!(locs.len(), 1, "{locs:?}");
        let defining_line = text
            .lines()
            .nth(locs[0].range.start.line as usize)
            .expect("the line jumped to");
        assert!(
            defining_line.contains("pod_namespace: my-namespace"),
            "landed on `{defining_line}`"
        );

        // The control that makes the two assertions above about `add_host` and not about
        // hover working at all: the consumed parameter beside it defines nothing, so
        // neither consumer may answer for it.
        let consumed = text.find("{{ name }}").unwrap() + 3;
        assert!(super::Backend::variable_hover_at(&doc, &nodes, consumed, &path, &no_buffers(), &[], &no_cache(), None).is_none());
        let (line, character) = doc.byte_to_lsp(consumed);
        assert!(super::Backend::definition_at(
            &doc,
            &nodes,
            tower_lsp::lsp_types::Position { line, character },
            &uri,
            &path
        , &no_buffers(),
                &[],
                            &no_cache(),
                            None,
            )
        .is_none());
    }

    /// T-066 against the real demo: `network_mtu` reaches the playbook only through
    /// provisioner's meta dependency on network-base, so its hover line carries the
    /// breadcrumb; `provisioner_user` comes from a role the playbook names directly, so
    /// its hover stays bare.
    /// T-166 end to end: the diagnostic itself, on the demo fixture, for every construct.
    /// The rule's logic was reachable only through a live `Backend` until this test forced
    /// it apart, which meant it had been checked by running `scan` by hand rather than
    /// pinned — the same "verified, not asserted" gap that let a stale binary and a wrong
    /// hover both survive earlier.
    #[test]
    fn the_mutated_condition_rule_reports_every_propagating_construct() {
        use tower_lsp::lsp_types::NumberOrString;
        let path = std::path::Path::new("../../demo/mutated_conditions.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).expect("demo fixture");
        let a = super::Backend::analyze_text(text.clone(), &path).unwrap();
        let ds = super::State::mutated_condition_diagnostics_with(&a, |t| {
            std::sync::Arc::new(ansible_core::mutation::mutated_vars(t))
        });
        assert!(
            ds.iter().all(|d| matches!(&d.code,
                Some(NumberOrString::String(s)) if s == "when-import-var-mutated")),
            "{ds:?}"
        );
        // One per BAD row and no more: the SILENCED row is suppressed, the two GOOD rows
        // either do not propagate or name a variable the target never assigns.
        let lines: Vec<u32> = ds.iter().map(|d| d.range.start.line + 1).collect();
        assert_eq!(lines.len(), 4, "{lines:?}");
        for (line, label) in lines.iter().zip(["roles: entry", "import_tasks", "import_role", "apply: when:"]) {
            let src_line = text.lines().nth(*line as usize - 1).unwrap_or("");
            assert!(src_line.contains("demo_done"), "{label} anchored wrong: {src_line}");
        }
        // The message has to carry the mechanism, not just name the variable.
        let m = &ds[0].message;
        assert!(m.contains("re-evaluated per task"), "{m}");
        assert!(m.contains("hosts can diverge"), "{m}");
    }

    /// T-100 regression: hover and the `var-undefined` diagnostic must agree about scope.
    /// They did not — the warning said `port_count` was "never defined" while the hover on
    /// the same token pointed at the definition, because the scope check lived in one
    /// caller instead of on the definition. Both halves are asserted here so a future
    /// consumer that forgets the rule fails rather than contradicting the other one.
    /// T-206, the hover consumer. The index and the resolver both feed this, so it is the
    /// surface where the two disagreeing was visible as a wrong answer rather than a silence.
    ///
    /// `tasks/extra.yml` is the decoy the old search order picked.
    #[test]
    fn hover_on_a_role_include_vars_name_points_at_the_role_vars_file() {
        let d = std::env::temp_dir().join("ansible-lsp-t206-hover");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("roles/ad/vars")).unwrap();
        std::fs::create_dir_all(d.join("roles/ad/tasks")).unwrap();
        std::fs::write(d.join("roles/ad/vars/extra.yml"), "other: FROM_ROLE_VARS_DIR\n").unwrap();
        std::fs::write(d.join("roles/ad/tasks/extra.yml"), "- debug: {msg: decoy}\n").unwrap();
        let path = d.join("roles/ad/tasks/main.yml");
        let text = "- include_vars: extra.yml\n- debug: {msg: \"{{ other }}\"}\n".to_string();
        std::fs::write(&path, &text).unwrap();

        let doc = ansible_core::parse::Document::new(text.clone());
        let nodes = doc.parse().unwrap();
        let byte = text.rfind("{{ other }}").unwrap() + 3;
        let h = super::Backend::variable_hover_at(
            &doc, &nodes, byte, &path, &no_buffers(), &[], &no_cache(), None,
        )
        .expect("hover answers for a name the include defines")
        .0;
        assert!(h.contains("include_vars"), "names the source: {h}");
        assert!(h.contains("FROM_ROLE_VARS_DIR"), "reads the role's vars file: {h}");
    }

    /// T-206, the `missing-file` consumer. `<project>/vars/` was absent from our candidate
    /// list altogether, so a vars file that Ansible loads was reported as a missing file —
    /// the tool contradicting a playbook that runs.
    #[test]
    fn include_vars_from_the_project_vars_dir_is_not_reported_missing() {
        let d = std::env::temp_dir().join("ansible-lsp-t206-missing");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("vars")).unwrap();
        std::fs::create_dir_all(d.join("plays")).unwrap();
        // `ansible.cfg` is what makes `d` the project root; without it there is no
        // project-relative candidate to find and the test would pass for the wrong reason.
        std::fs::write(d.join("ansible.cfg"), "[defaults]\n").unwrap();
        std::fs::write(d.join("vars/shared.yml"), "shared_key: 1\n").unwrap();
        let path = d.join("plays/p.yml");
        let text = "- hosts: all\n  tasks:\n    - include_vars: shared.yml\n".to_string();
        std::fs::write(&path, &text).unwrap();

        let a = super::Backend::analyze_text(text, &path).unwrap();
        let missing: Vec<_> = super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter(|d| matches!(&d.code, Some(tower_lsp::lsp_types::NumberOrString::String(s)) if s == "missing-file"))
            .collect();
        assert!(missing.is_empty(), "project vars/ resolves: {missing:#?}");
    }

    #[test]
    fn hover_and_the_undefined_warning_agree_about_entry_scope() {
        let path = std::path::Path::new("../../demo/role_params.yml").canonicalize().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = ansible_core::parse::Document::new(text.clone());
        let nodes = doc.parse().unwrap();
        let hover_at_last = |name: &str| {
            let byte = text.rfind(&format!("{{{{ {name} }}}}")).unwrap() + 3;
            super::Backend::variable_hover_at(&doc, &nodes, byte, &path, &no_buffers(), &[], &no_cache(), None).map(|h| h.0)
        };

        // Used inside its own entry: in scope, so the definition is offered.
        let in_scope = hover_at_last("app_env").expect("in-scope hover");
        assert!(in_scope.contains("role param"), "{in_scope}");
        assert!(in_scope.contains("staging"), "{in_scope}");

        // Used in the play's tasks: out of scope, so hover must decline — anything else
        // contradicts the warning on the same token.
        assert_eq!(hover_at_last("port_count"), None);

        // ...and that warning names the real problem instead of "never defined".
        let a = super::Backend::analyze_text(text.clone(), &path).unwrap();
        let msgs: Vec<String> = super::Backend::variable_coverage_diagnostics(&a, &path, &a.nodes, &[], &no_cache())
            .into_iter()
            .map(|d| d.message)
            .collect();
        let hit = msgs
            .iter()
            .find(|m| m.contains("`port_count`"))
            .unwrap_or_else(|| panic!("no port_count warning in {msgs:?}"));
        assert!(hit.contains("not the play's tasks"), "{hit}");
        assert!(!hit.contains("never defined"), "{hit}");
    }

    /// T-104 against `demo/hostvars.yml`, both halves at once — which is the point. The
    /// GOOD rows were broken in the same way the BAD ones were: `hostvars[h].x` produced
    /// no use at all, so there was nothing to navigate *and* nothing to judge. One
    /// extraction fixes both, and hover must agree with the warning on every row or we
    /// have rebuilt the T-100 contradiction one rule over.
    #[test]
    fn hostvars_reads_navigate_where_visible_and_warn_where_not() {
        let path = std::path::Path::new("../../demo/hostvars.yml").canonicalize().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = ansible_core::parse::Document::new(text.clone());
        let nodes = doc.parse().unwrap();
        let uri = tower_lsp::lsp_types::Url::from_file_path(&path).unwrap();
        // The name inside `hostvars['web01'].<name>`, which is what T-104 had to extract.
        let at = |needle: &str| text.find(needle).unwrap() + needle.find("].").unwrap() + 2;

        // VISIBLE: host_vars belongs to the host, so both views answer. `web01_ib_ip` is
        // defined in exactly one place, which is what makes this row readable — `app_port`
        // is defined in three and its value depends on group membership we cannot know.
        {
            let byte = at("'web01'].web01_ib_ip");
            let md = super::Backend::variable_hover_at(&doc, &nodes, byte, &path, &no_buffers(), &[], &no_cache(), None)
                .expect("no hover on the host_vars read")
                .0;
            assert!(md.contains("10.0.0.1"), "hover shows the value: {md}");
            let (line, character) = doc.byte_to_lsp(byte);
            let pos = tower_lsp::lsp_types::Position { line, character };
            assert!(
                super::Backend::variable_defs_at(&doc, &nodes, pos, &uri, &no_buffers(), &[], &no_cache(), None).is_some(),
                "no jump target on the host_vars read"
            );
        }

        // INVISIBLE: a play var. Hover declines and go-to-definition declines, because
        // offering the play var would point at a value this read can never produce.
        let byte = at("'web01'].play_scoped");
        assert!(super::Backend::variable_hover_at(&doc, &nodes, byte, &path, &no_buffers(), &[], &no_cache(), None).is_none());
        let (line, character) = doc.byte_to_lsp(byte);
        let pos = tower_lsp::lsp_types::Position { line, character };
        assert!(super::Backend::variable_defs_at(&doc, &nodes, pos, &uri, &no_buffers(), &[], &no_cache(), None).is_none());

        // ...and the warning takes over on exactly the two BAD rows, naming the source it
        // found rather than claiming the variable was never defined.
        let a = super::Backend::analyze_text(text.clone(), &path).unwrap();
        // NOTHING is reported on a hostvars read — the retraction. The obvious rule
        // ("every definition I can see is play-scoped, so this is always undefined") is
        // unsound while inventory is unparsed: a name in play `vars:` AND in inventory
        // reads fine through hostvars, measured. So the demo's BAD rows are BAD about
        // *Ansible*, and we stay quiet about them until T-062.
        let ds = super::Backend::variable_coverage_diagnostics(&a, &path, &a.nodes, &[], &no_cache());
        let msgs: Vec<String> = ds.into_iter().map(|d| d.message).collect();
        assert!(msgs.is_empty(), "no claim about a hostvars read: {msgs:?}");
        // The control that keeps that silence meaningful: the same check is alive in this
        // file for an ordinary read, so the quiet above is the rule and not a dead pass.
        let live = super::Backend::analyze_text(
            "- hosts: all\n  tasks:\n    - debug: { msg: \"{{ nowhere_at_all }}\" }\n".into(),
            &path,
        )
        .unwrap();
        assert_eq!(
            super::Backend::variable_coverage_diagnostics(&live, &path, &live.nodes, &[], &no_cache()).len(),
            1
        );
        let inv = at("'web01'].infiniband_ip");
        assert!(super::Backend::variable_hover_at(&doc, &nodes, inv, &path, &no_buffers(), &[], &no_cache(), None).is_none());

        // T-171, the other half of the same line: the HOST key. `host_vars/web01.yml` is a
        // deterministic path — the filename is the host name — so this resolves without an
        // inventory. Asserted here rather than apart, because the row invites one click per
        // name and a reader finding only one of them working reads it as broken.
        // Through `definition_at`, the whole Cmd+click chain — not `host_key_defs_at`
        // alone. Calling the helper is what let the missing paint ship.
        let key = text.find("hostvars['web01']").unwrap() + "hostvars['".len() + 1;
        let (line, character) = doc.byte_to_lsp(key);
        let pos = tower_lsp::lsp_types::Position { line, character };
        let locs = super::Backend::definition_at(&doc, &nodes, pos, &uri, &path, &no_buffers(), &[], &no_cache(), None)
            .expect("host key jumps");
        assert_eq!(locs.len(), 1);
        assert!(
            locs[0].uri.path().ends_with("demo/host_vars/web01.yml"),
            "landed on {}",
            locs[0].uri
        );
        // ...and it is PAINTED, not just clickable. Go-to-definition alone shipped a
        // feature with no visual affordance: the first thing tried in the editor was
        // "'web01' isn't coloured", because nothing said it could be clicked.
        // Through `document_links_of`, so this covers what the editor is sent.
        let a_links = super::Backend::analyze_text(text.clone(), &path).unwrap();
        let all = super::Backend::document_links_of(&a_links, &uri);
        let painted: Vec<_> = all
            .into_iter()
            .filter(|l| l.target.as_ref().is_some_and(|t| t.path().ends_with("host_vars/web01.yml")))
            .collect();
        assert!(!painted.is_empty(), "the host key must paint like a path");
        for l in &painted {
            assert!(
                l.target.as_ref().unwrap().path().ends_with("demo/host_vars/web01.yml"),
                "painted link points at {:?}",
                l.target
            );
            // The range covers the host name only — not the quotes, not `hostvars[`.
            let line = text.lines().nth(l.range.start.line as usize).unwrap();
            let painted_text: String = line
                .chars()
                .skip(l.range.start.character as usize)
                .take((l.range.end.character - l.range.start.character) as usize)
                .collect();
            assert_eq!(painted_text, "web01");
        }
        // Only the keys that resolve are painted: `inventory_hostname` is not a literal,
        // and a literal naming a host with no host_vars file has nothing to point at.
        assert_eq!(painted.len(), text.matches("hostvars['web01']").count());

        // A non-literal key names no host we can know — silent, not a guess.
        let dyn_key = text.find("hostvars[inventory_hostname]").unwrap() + "hostvars[".len() + 1;
        let (line, character) = doc.byte_to_lsp(dyn_key);
        let pos = tower_lsp::lsp_types::Position { line, character };
        assert!(super::Backend::host_key_defs_at(&doc, pos, &path).is_none());
        // The variable half of the same line still wins its own click — the host-key
        // branch is last in the chain and must not shadow it.
        let (line, character) = doc.byte_to_lsp(at("'web01'].web01_ib_ip"));
        let pos = tower_lsp::lsp_types::Position { line, character };
        let v = super::Backend::definition_at(&doc, &nodes, pos, &uri, &path, &no_buffers(), &[], &no_cache(), None).expect("var jumps");
        assert!(v[0].uri.path().ends_with("demo/host_vars/web01.yml"));

        for m in &msgs {
            assert!(m.starts_with("always undefined:"), "verdict first: {m}");
            assert!(m.contains("`play_scoped`"), "{m}");
            assert!(m.contains("play var"), "names the source it found: {m}");
            assert!(m.contains("without the play"), "gives the mechanism: {m}");
            assert!(m.contains("Move the value"), "gives the fix: {m}");
            assert!(!m.contains("never defined"), "the definition exists: {m}");
        }
    }

    /// T-169's two editor consumers, against the demo rows that claim them. A name inside
    /// a key Ansible renders (`set_fact`, `set_stats`' `data:`) hovers and Cmd+clicks like
    /// any other; the same name in a key Ansible leaves literal must do neither, or we
    /// point at a definition the run never reaches. The demo labels those rows GOOD and
    /// NO HINT, and this is what makes those labels true.
    #[test]
    fn a_name_in_a_rendered_key_hovers_and_jumps_and_a_literal_one_does_not() {
        let path = std::path::Path::new("../../demo/tasks/variables.yml").canonicalize().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = ansible_core::parse::Document::new(text.clone());
        let nodes = doc.parse().unwrap();
        let uri = tower_lsp::lsp_types::Url::from_file_path(&path).unwrap();
        let at = |byte: usize| {
            let (line, character) = doc.byte_to_lsp(byte);
            tower_lsp::lsp_types::Position { line, character }
        };

        // Each row's `"{{ result_name }}"` key, found by the line that precedes it.
        let key_on = |anchor: &str| {
            let at = text.find(anchor).expect("demo row present");
            text[at..].find("result_name").expect("templated key") + at + 1
        };
        let set_fact_key = key_on("ansible.builtin.set_fact:\n        \"{{ result_name }}\"");
        let set_stats_key = key_on("data:\n          \"{{ result_name }}_count\"");
        let nested_key = key_on("nested_map:");

        for (byte, what) in [(set_fact_key, "set_fact"), (set_stats_key, "set_stats data")] {
            let md = super::Backend::variable_hover_at(&doc, &nodes, byte, &path, &no_buffers(), &[], &no_cache(), None)
                .unwrap_or_else(|| panic!("no hover on the {what} key"))
                .0;
            assert!(md.contains("my_result"), "{what} hover shows the play var: {md}");

            let locs = super::Backend::variable_defs_at(&doc, &nodes, at(byte), &uri, &no_buffers(), &[], &no_cache(), None)
                .unwrap_or_else(|| panic!("no jump target on the {what} key"));
            // Jumps to the play var it reads, not to the key it sits in.
            assert_eq!(locs.len(), 1, "{what}: {locs:?}");
            let line = locs[0].range.start.line as usize;
            assert!(
                text.lines().nth(line).unwrap().contains("result_name: my_result"),
                "{what} landed on: {}",
                text.lines().nth(line).unwrap()
            );
        }

        // The boundary row: nested in a fact's value the braces are data, so both views
        // stay silent rather than claiming a name Ansible keeps literal.
        assert!(super::Backend::variable_hover_at(&doc, &nodes, nested_key, &path, &no_buffers(), &[], &no_cache(), None).is_none());
        assert!(super::Backend::variable_defs_at(&doc, &nodes, at(nested_key), &uri, &no_buffers(), &[], &no_cache(), None).is_none());
    }

    #[test]
    fn hover_breadcrumbs_meta_dependency_routes_only() {
        let path = std::path::Path::new("../../demo/cross_file_vars.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = ansible_core::parse::Document::new(text.clone());
        let nodes = doc.parse().unwrap();

        let hover = |name: &str| {
            let byte = text.find(&format!("{{{{ {name} }}}}")).unwrap() + 3;
            super::Backend::variable_hover_at(&doc, &nodes, byte, &path, &no_buffers(), &[], &no_cache(), None)
                .expect("hover expected")
                .0
        };
        let mtu = hover("network_mtu");
        assert!(plain(&mtu).contains("dependency of provisioner"), "no breadcrumb in: {mtu}");
        assert!(mtu.contains("provisioner/meta/main.yml"), "wrong edge in: {mtu}");
        assert!(!hover("provisioner_user").contains("dependency of"));
    }

    /// T-066 chain rendering against demo/dependency_chain.yml: depth N shows exactly the
    /// N meta hops, outermost first (chain-a/meta → … ), depth 0 none.
    #[test]
    fn hover_renders_the_full_dependency_chain_per_depth() {
        let path = std::path::Path::new("../../demo/dependency_chain.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = ansible_core::parse::Document::new(text.clone());
        let nodes = doc.parse().unwrap();

        for depth in 0..=5usize {
            let name = format!("chain_depth{depth}");
            let byte = text.find(&format!("{{{{ {name} }}}}")).unwrap() + 3;
            let (md, _) = super::Backend::variable_hover_at(&doc, &nodes, byte, &path, &no_buffers(), &[], &no_cache(), None)
                .expect("hover expected");
            println!("--- {name} ---\n{md}\n");
            // One nested "dependency of" line per hop.
            let hops = md.matches("dependency of").count();
            assert_eq!(hops, depth, "depth {depth} should render {depth} hops:\n{md}");
            // Innermost first: the requirer of the defining role at the top, the role this
            // playbook names (chain-a) at the bottom.
            if depth > 0 {
                let innermost = format!("chain-{}/meta/main.yml", (b'a' + depth as u8 - 1) as char);
                assert!(
                    md.find(&innermost).unwrap() < md.find("chain-a/meta/main.yml").unwrap()
                        || depth == 1,
                    "stack must read innermost-first:\n{md}"
                );
            }
        }

        // include_vars through the chain: mechanism on the def line, route underneath.
        let byte = text.find("{{ chain_included }}").unwrap() + 3;
        let (md, _) = super::Backend::variable_hover_at(&doc, &nodes, byte, &path, &no_buffers(), &[], &no_cache(), None)
            .expect("hover expected");
        println!("--- chain_included ---\n{md}\n");
        assert!(md.contains("include_vars"), "wrong source:\n{md}");
        assert!(md.contains("chain-c/vars/settings.yml"), "wrong file:\n{md}");
        assert!(plain(&md).contains("dependency of chain-b"), "missing inner hop:\n{md}");
        assert!(plain(&md).contains("dependency of chain-a"), "missing outer hop:\n{md}");
    }

    /// T-208 through the surface it is visible on. The same name, hovered either side of the
    /// include that re-loads it: above, only the auto-loaded role var has happened; below,
    /// both levels have and the include is the one in effect.
    ///
    /// Before T-208 both positions rendered the lower pane — `include_vars ← effective` — so
    /// the top half is the assertion that fails without the fix.
    #[test]
    fn hover_above_the_include_does_not_claim_the_include_has_run() {
        let d = std::env::temp_dir().join("ansible-lsp-t208-hover");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("roles/ad/vars")).unwrap();
        std::fs::create_dir_all(d.join("roles/ad/tasks")).unwrap();
        std::fs::write(d.join("roles/ad/vars/main.yml"), "thing: X\n").unwrap();
        let path = d.join("roles/ad/tasks/main.yml");
        let text = concat!(
            "- debug: {msg: \"above {{ thing }}\"}\n",
            "- include_vars: main.yml\n",
            "- debug: {msg: \"below {{ thing }}\"}\n",
        )
        .to_string();
        std::fs::write(&path, &text).unwrap();
        let doc = ansible_core::parse::Document::new(text.clone());
        let nodes = doc.parse().unwrap();
        let at = |label: &str| {
            let byte = text.find(&format!("{label} {{{{ thing }}}}")).unwrap() + label.len() + 4;
            plain(
                &super::Backend::variable_hover_at(
                    &doc, &nodes, byte, &path, &no_buffers(), &[], &no_cache(), None,
                )
                .expect("hover answers")
                .0,
            )
        };

        let above = at("above");
        assert!(above.contains("role var"), "the level that has loaded:\n{above}");
        assert!(
            !above.contains("include_vars"),
            "the include is below this use and has not run:\n{above}"
        );
        assert!(!above.contains("2 definitions"), "one level applies here:\n{above}");

        let below = at("below");
        assert!(below.contains("2 definitions"), "both, once the include has run:\n{below}");
        let inc = below.find("include_vars").expect("the effective level");
        let role = below.find("role var").expect("and the one it outranks");
        assert!(inc < role, "include_vars leads below the include:\n{below}");
    }

    fn codes_of(
        text: &str,
        path: &std::path::Path,
    ) -> Vec<(String, Option<tower_lsp::lsp_types::DiagnosticSeverity>)> {
        use tower_lsp::lsp_types::NumberOrString;
        let a = super::Backend::analyze_text(text.to_string(), path).expect("analysed");
        super::Backend::diagnostics_of(&a)
            .into_iter()
            .filter_map(|d| match d.code {
                Some(NumberOrString::String(c)) => Some((c, d.severity)),
                _ => None,
            })
            .collect()
    }

    fn role_tree(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("roles/ad/vars")).unwrap();
        std::fs::create_dir_all(d.join("roles/ad/tasks")).unwrap();
        std::fs::create_dir_all(d.join("roles/other/vars")).unwrap();
        std::fs::write(d.join("roles/ad/vars/main.yml"), "thing: v\n").unwrap();
        std::fs::write(d.join("roles/ad/vars/extra.yml"), "other: v\n").unwrap();
        std::fs::write(d.join("roles/other/vars/main.yml"), "far: v\n").unwrap();
        d
    }

    /// T-184 end to end: the diagnostic exists, is a HINT, and carries its own id.
    #[test]
    fn a_role_reloading_its_own_vars_is_a_hint_not_a_warning() {
        let d = role_tree("ansible-lsp-t184-fires");
        let path = d.join("roles/ad/tasks/main.yml");
        let text = "- include_vars: main.yml\n";
        std::fs::write(&path, text).unwrap();

        let got = codes_of(text, &path);
        let hit = got
            .iter()
            .find(|(c, _)| c == "redundant-role-vars-include")
            .unwrap_or_else(|| panic!("not reported: {got:?}"));
        assert_eq!(
            hit.1,
            Some(tower_lsp::lsp_types::DiagnosticSeverity::HINT),
            "legal code gets no squiggle"
        );
    }

    /// Every silence case as its own assertion. One combined test would pass with three of the
    /// four guards missing, which is the failure mode the ticket names.
    #[test]
    fn the_role_vars_hint_stays_silent_where_nothing_is_provable() {
        let d = role_tree("ansible-lsp-t184-silent");
        let inside = d.join("roles/ad/tasks/main.yml");
        let outside = d.join("play.yml");
        std::fs::write(&outside, "- hosts: all\n").unwrap();

        for (label, text, path) in [
            ("a different file in the same role", "- include_vars: extra.yml\n", &inside),
            ("another role's vars/main.yml", "- include_vars: ../../other/vars/main.yml\n", &inside),
            ("the dir form", "- include_vars: {dir: vars}\n", &inside),
            ("a file that does not resolve", "- include_vars: nope.yml\n", &inside),
            ("the same name from outside any role", "- include_vars: main.yml\n", &outside),
        ] {
            std::fs::write(path, text).unwrap();
            let got = codes_of(text, path);
            assert!(
                !got.iter().any(|(c, _)| c == "redundant-role-vars-include"),
                "{label}: should be silent, got {got:?}"
            );
        }
    }

    /// T-010: the rule answers to its own id and to no other.
    #[test]
    fn the_role_vars_hint_is_noqa_suppressible_by_its_own_id() {
        let d = role_tree("ansible-lsp-t184-noqa");
        let path = d.join("roles/ad/tasks/main.yml");
        let fires = |text: &str| {
            std::fs::write(&path, text).unwrap();
            codes_of(text, &path).iter().any(|(c, _)| c == "redundant-role-vars-include")
        };

        assert!(fires("- include_vars: main.yml\n"), "control: it fires unsuppressed");
        assert!(!fires("- include_vars: main.yml  # noqa: redundant-role-vars-include\n"));
        // Somebody else's id must not silence it, or one suppression would hide two rules.
        assert!(fires("- include_vars: main.yml  # noqa: missing-file\n"));
    }

    /// Rule 4: `demo/roles/chain-c/tasks/main.yml` is labelled HINT for this rule. Asserted,
    /// and paired with a sweep so the rule cannot start firing on demo files that carry no
    /// such label without a test noticing.
    #[test]
    fn the_demo_hint_row_fires_and_no_other_demo_file_does() {
        let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
        let labelled = demo.join("roles/chain-c/tasks/main.yml");
        let text = std::fs::read_to_string(&labelled).unwrap();
        assert!(
            codes_of(&text, &labelled).iter().any(|(c, _)| c == "redundant-role-vars-include"),
            "the labelled row must actually hint"
        );

        for f in ansible_core::workspace::yaml_files(&demo) {
            if f == labelled {
                continue;
            }
            let Ok(t) = std::fs::read_to_string(&f) else { continue };
            let Some(a) = super::Backend::analyze_text(t, &f) else { continue };
            let hit = super::Backend::diagnostics_of(&a).into_iter().any(|d| {
                matches!(&d.code,
                    Some(tower_lsp::lsp_types::NumberOrString::String(c))
                        if c == "redundant-role-vars-include")
            });
            assert!(!hit, "unlabelled demo file started hinting: {}", f.display());
        }
    }

    /// T-184's corpus gate, kept runnable rather than done once and described in a ticket.
    ///
    /// `ANSIBLE_CORPUS=<dir> cargo test -p ansible-lsp redundant_role_vars_corpus -- --ignored --nocapture`
    ///
    /// Point it at one tree at a time. The trees the gate was measured against, pinned so the
    /// result can be reproduced rather than taken on faith — same convention as the `when:`
    /// corpus in `condition.rs`:
    ///
    /// | tree | commit | hits |
    /// | ------------------------------------------ | --------- | ---- |
    /// | `kubernetes-sigs/kubespray`                | `46dbdd3` | 0 |
    /// | `ansible/ansible`                          | `b85437b` | 0 |
    /// | `ansible-collections/community.general`    | `0bf15b1` | 0 |
    /// | `debops/debops`                            | `65b66ff` | 0 |
    /// | `openstack/openstack-ansible`              | `3dcf546` | 0 |
    /// | `ansible/ansible-examples`                 | `b505865` | 0 |
    /// | `geerlingguy/ansible-role-mysql`           | `0a0ea6b` | 0 |
    /// | `sovereign/sovereign`                      | `9fd5ff5` | 0 |
    ///
    /// ```text
    /// git clone --depth 1 https://github.com/kubernetes-sigs/kubespray.git
    /// ```
    ///
    /// **Eight zeros is also what a broken sweep looks like**, so run a control that must come
    /// out different before believing them: this repo's own `demo/` reports exactly 1, on the
    /// row labelled for the rule. If that comes back 0, the sweep is broken, not the corpus.
    ///
    /// Prints every hit with its file and line, because the gate is "read each one" — a bare
    /// count cannot tell a common idiom from a rule that has started guessing. Ignored by
    /// default and env-gated so no corpus path is ever written into this repo.
    #[test]
    #[ignore = "corpus gate: ANSIBLE_CORPUS=<path> cargo test -p ansible-lsp redundant_role_vars_corpus -- --ignored --nocapture"]
    fn redundant_role_vars_corpus() {
        let Ok(root) = std::env::var("ANSIBLE_CORPUS") else { return };
        let root = std::path::PathBuf::from(root);
        if !root.is_dir() {
            return;
        }
        let (mut hits, mut roles, mut files) = (Vec::new(), std::collections::BTreeSet::new(), 0);
        for f in ansible_core::workspace::yaml_files(&root) {
            let Ok(t) = std::fs::read_to_string(&f) else { continue };
            if !t.contains("include_vars") {
                continue;
            }
            files += 1;
            let Some(a) = super::Backend::analyze_text(t, &f) else { continue };
            if let Some(r) = &a.ctx.role_dir {
                roles.insert(r.clone());
            }
            for d in super::Backend::diagnostics_of(&a) {
                if matches!(&d.code, Some(tower_lsp::lsp_types::NumberOrString::String(c))
                    if c == "redundant-role-vars-include")
                {
                    hits.push(format!(
                        "{}:{}",
                        f.strip_prefix(&root).unwrap_or(&f).display(),
                        d.range.start.line + 1
                    ));
                }
            }
        }
        println!(
            "redundant-role-vars-include: {} hit(s); {files} files use include_vars, across {} roles",
            hits.len(),
            roles.len()
        );
        for h in &hits {
            println!("  HIT {h}");
        }
    }

    /// T-207, the hover consumer — and the assertion that separates the fix that landed from
    /// the one that was rejected. Collapsing to the winning level would render a single row
    /// reading `include_vars`, with nothing saying the file is auto-loaded as well; the whole
    /// point of the stack is that a reader sees both routes and which one wins.
    #[test]
    fn hover_shows_both_levels_of_a_role_that_re_includes_its_own_vars() {
        let d = std::env::temp_dir().join("ansible-lsp-t207-hover");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("roles/ad/vars")).unwrap();
        std::fs::create_dir_all(d.join("roles/ad/tasks")).unwrap();
        std::fs::write(d.join("roles/ad/vars/main.yml"), "thing: FROM_ROLE_VARS\n").unwrap();
        let path = d.join("roles/ad/tasks/main.yml");
        let text = "- include_vars: main.yml\n- debug: {msg: \"{{ thing }}\"}\n".to_string();
        std::fs::write(&path, &text).unwrap();

        let doc = ansible_core::parse::Document::new(text.clone());
        let nodes = doc.parse().unwrap();
        let byte = text.rfind("{{ thing }}").unwrap() + 3;
        let (md, _) = super::Backend::variable_hover_at(
            &doc, &nodes, byte, &path, &no_buffers(), &[], &no_cache(), None,
        )
        .expect("hover answers");
        let flat = plain(&md);

        assert!(flat.contains("2 definitions"), "both routes counted:\n{flat}");
        assert!(flat.contains("include_vars"), "the level in effect:\n{flat}");
        assert!(flat.contains("role var"), "and why the file is in scope at all:\n{flat}");
        // Order is the claim: the effective one leads, and it is the include.
        let inc = flat.find("include_vars").unwrap();
        let role = flat.find("role var").unwrap();
        assert!(inc < role, "include_vars must lead the stack:\n{flat}");
        assert!(flat.contains("effective"), "the winner is marked:\n{flat}");
    }

    /// T-207, rule 4: `demo/roles/chain-c/tasks/main.yml` claims hovering `chain_c_tuning`
    /// shows two definitions of one file, `include_vars` leading and marked effective. That
    /// label is a claim about our own output, so it is asserted rather than trusted.
    #[test]
    fn the_demo_role_that_re_includes_its_own_vars_hovers_both_levels() {
        let path = std::path::Path::new("../../demo/roles/chain-c/tasks/main.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = ansible_core::parse::Document::new(text.clone());
        let nodes = doc.parse().unwrap();
        let byte = text.rfind("{{ chain_c_tuning }}").unwrap() + 3;
        let (md, _) = super::Backend::variable_hover_at(
            &doc, &nodes, byte, &path, &no_buffers(), &[], &no_cache(), None,
        )
        .expect("hover answers for the twice-loaded name");
        let flat = plain(&md);

        assert!(flat.contains("2 definitions"), "the demo says two:\n{flat}");
        let inc = flat.find("include_vars").expect("the effective level");
        let role = flat.find("role var").expect("why the file is in scope");
        assert!(inc < role, "include_vars leads, as the demo says:\n{flat}");
        assert!(flat.contains("effective"), "and is marked:\n{flat}");
    }

    /// T-206, rule 4: `demo/roles/chain-c/tasks/main.yml` labels its include **GOOD**, saying
    /// it resolves to `vars/settings.yml` and not to the same-named file beside it. That label
    /// is a claim, so it is asserted here rather than trusted.
    ///
    /// The decoy is checked first. Without it the include resolves correctly under either
    /// search order, the demo demonstrates nothing, and this test passes while proving
    /// nothing — which is exactly how the bug survived in the demo tree for as long as it did.
    #[test]
    fn the_demo_role_include_vars_resolves_past_its_task_dir_namesake() {
        let role = std::path::Path::new("../../demo/roles/chain-c").canonicalize().unwrap();
        assert!(
            role.join("tasks/settings.yml").is_file(),
            "the decoy is the whole point of this fixture — restore it, don't delete this test"
        );
        assert!(role.join("vars/settings.yml").is_file(), "the file the include really means");

        let path = role.join("tasks/main.yml");
        let text = std::fs::read_to_string(&path).unwrap();
        let a = super::Backend::analyze_text(text, &path).unwrap();
        let (_, res) = a
            .refs
            .iter()
            .find(|(r, _)| r.kind == ansible_core::references::ReferenceKind::IncludeVars)
            .expect("the include_vars reference");
        assert_eq!(
            res.targets,
            vec![role.join("vars/settings.yml")],
            "the role's vars/ dir is searched before the including file's own dir"
        );
    }

    /// T-218: the hover on a role that has no `tasks/main.yml` must not claim the role is
    /// somewhere else.
    ///
    /// This is the surface the bug was actually visible on, so it is asserted here and not
    /// only on `skip_reason` — "a test can reach this code" and "this value reaches that
    /// code" are different questions, and T-178 was written after scoping only the first.
    #[test]
    fn a_role_without_main_tasks_hovers_as_present_not_as_installed_elsewhere() {
        let root = std::env::temp_dir().join("t218-hover");
        let _ = std::fs::remove_dir_all(&root);
        let w = |rel: &str, text: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        w("ansible.cfg", "[defaults]\nroles_path = ./roles\n");
        // No tasks/main.yml, which is the whole point.
        w("roles/batch-window/tasks/begin.yml", "- debug: {msg: begin}\n");
        w("roles/withmain/tasks/main.yml", "- debug: {msg: main}\n");
        w(
            "playbooks/site.yml",
            "- hosts: all\n  tasks:\n\
             \x20   - include_role: { name: batch-window, tasks_from: begin }\n\
             \x20   - include_role: { name: withmain, tasks_from: main }\n",
        );

        let path = root.join("playbooks/site.yml").canonicalize().unwrap();
        let doc = ansible_core::parse::Document::new(std::fs::read_to_string(&path).unwrap());
        let nodes = doc.parse().unwrap();
        let ctx = ansible_core::workspace::FileContext::discover(&path);
        let refs = ansible_core::references::extract(&nodes).refs;
        let hover = |name: &str| {
            let r = refs
                .iter()
                .find(|r| r.value == name && r.kind == ansible_core::references::ReferenceKind::Role)
                .expect("role ref");
            let res = ansible_core::resolve::Resolver {
                literals: Some(&Default::default()),
                ..Default::default()
            }
            .resolve(r, &ctx);
            super::reference_hover(r, &res, &ctx, false).map(crate::Md::render)
        };

        let md = hover("batch-window").expect("hover expected");
        assert!(
            !md.contains("not in this workspace"),
            "the role is in the workspace: {md}"
        );
        assert!(md.contains("tasks/main.yml"), "say what is actually absent: {md}");
        assert!(md.contains("batch-window"), "and name where the role is: {md}");

        // The control that makes the assertion above mean something: a role WITH main.yml,
        // called the same way, resolves and so takes a different branch entirely. Without
        // this, a hover that said the same thing for every role would pass.
        let with_main = hover("withmain").unwrap_or_default();
        assert!(
            !with_main.contains("no `tasks/main.yml`"),
            "a role that has main.yml must not get the skipped hover: {with_main}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// T-029 against the real demo: a templated path whose variables have no known value
    /// still hovers — the glob-matched targets, listed without a winner — and one that
    /// matches nothing says why it goes nowhere instead of staying silent.
    #[test]
    fn hover_lists_glob_targets_for_unknown_value_templated_paths() {
        let path = std::path::Path::new("../../demo/tasks/main.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = ansible_core::parse::Document::new(text);
        let nodes = doc.parse().unwrap();
        let ctx = ansible_core::workspace::FileContext::discover(&path);
        let refs = ansible_core::references::extract(&nodes).refs;
        let hover = |value: &str| {
            let r = refs.iter().find(|r| r.value == value).expect("ref in demo");
            let res = ansible_core::resolve::Resolver { literals: Some(&Default::default()), ..Default::default() }
            .resolve(r, &ctx);
            super::reference_hover(r, &res, &ctx, false).map(crate::Md::render)
        };

        let md = hover("{{ protocol }}_target/check.yml").expect("hover expected");
        assert!(md.contains("2 possible targets"), "count in: {md}");
        assert!(
            md.contains("http_target/check.yml") && md.contains("ftp_target/check.yml"),
            "both targets in: {md}"
        );
        assert!(!md.contains("won"), "no winner to mark in: {md}");

        // Zero glob matches, and the anchor-less pattern that deliberately offers none:
        // both must explain themselves rather than hover nothing.
        for value in ["{{ protocol }}_target/nope.yml", "{{ anything }}.yml"] {
            let md = hover(value).expect("hover expected");
            assert!(md.contains("Skipped"), "skip reason for {value} in: {md}");
        }
    }

    /// `ansibleLsp.inventory` parsing, including every shape a settings blob can be wrong in.
    ///
    /// The filtering is the point: a blank entry reaching the resolver would name the
    /// workspace root as an inventory, and a non-string one would be silently dropped by
    /// `as_str` anyway — better to know which.
    #[test]
    fn the_inventory_setting_keeps_only_usable_paths() {
        // Built inline rather than deriving `Default` on `State`: the production struct is
        // constructed once, in `main`, and widening its API for a test is the wrong trade.
        let state = super::State {
            docs: std::sync::Mutex::new(std::collections::HashMap::new()),
            roots: std::sync::Mutex::new(Vec::new()),
            flagged: std::sync::Mutex::new(std::collections::HashSet::new()),
            render_sites: Default::default(),
            template_grammars: Default::default(),
            mutations: std::sync::Mutex::new(std::collections::HashMap::new()),
            settings: std::sync::Mutex::new(Default::default()),
            inventory: std::sync::Mutex::new(Vec::new()),
            var_cache: no_cache(),
            scan_task: Default::default(),
            ansible_path: Default::default(),
            install: Default::default(),
            startup_note: std::sync::Mutex::new(String::new()),
            scanning: std::sync::atomic::AtomicBool::new(false),
        };
        let set = |v: serde_json::Value| {
            state.set_inventory(&v);
            state.inventory.lock().unwrap().clone()
        };

        assert_eq!(
            set(serde_json::json!({"inventory": ["a.ini", "dir/b.yml"]})),
            [std::path::PathBuf::from("a.ini"), std::path::PathBuf::from("dir/b.yml")]
        );
        // Blank and whitespace-only entries are dropped: either would resolve to the
        // workspace root and make every file in it an inventory source.
        assert_eq!(
            set(serde_json::json!({"inventory": ["", "   ", "real.ini"]})),
            [std::path::PathBuf::from("real.ini")]
        );
        // Non-strings are dropped rather than stringified.
        assert_eq!(
            set(serde_json::json!({"inventory": [1, true, null, "keep.ini"]})),
            [std::path::PathBuf::from("keep.ini")]
        );
        // Every shape that means "nothing set" ends empty rather than erroring.
        for v in [
            serde_json::json!({}),
            serde_json::json!({"inventory": null}),
            serde_json::json!({"inventory": "not-an-array"}),
            serde_json::json!({"inventory": []}),
        ] {
            assert!(set(v.clone()).is_empty(), "{v} should clear the setting");
        }

        // Setting it bumps the cache epoch, because a changed inventory changes what every
        // file can see and nothing computed under the old one may survive.
        let before = state.var_cache.lock().unwrap().epoch;
        state.set_inventory(&serde_json::json!({"inventory": ["x.ini"]}));
        let after = state.var_cache.lock().unwrap().epoch;
        assert_ne!(before, after, "a changed inventory must invalidate wholesale");

        state.set_inventory(&serde_json::json!({}));
    }

    /// A templated path that resolved by **substitution** explains itself: what it points
    /// at, and for each variable the value used and where that value came from.
    ///
    /// "Why does this go to prod.yml" is the question, and the answer is only useful if it
    /// names the definition — so the assertions are on the value, the source label and the
    /// link, not merely on the target.
    #[test]
    fn a_substituted_path_hovers_the_value_and_where_it_came_from() {
        let d = std::env::temp_dir().join("ansible-lsp-subst-hover");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("tasks")).unwrap();
        std::fs::write(d.join("ansible.cfg"), "[defaults]\n").unwrap();
        std::fs::write(d.join("tasks/prod.yml"), "- debug:\n    msg: hi\n").unwrap();
        let play = d.join("play.yml");
        let text = "- hosts: all\n  vars:\n    env: prod\n  tasks:\n                        - include_tasks: \"tasks/{{ env }}.yml\"\n";
        std::fs::write(&play, text).unwrap();

        let doc = ansible_core::parse::Document::new(text.to_string());
        let nodes = doc.parse().unwrap();
        let ctx = ansible_core::workspace::FileContext::discover(&play);
        let refs = ansible_core::references::extract(&nodes).refs;
        let r = refs.iter().find(|r| r.value.contains("{{ env }}")).expect("the templated ref");
        let res = ansible_core::resolve::Resolver { literals: Some(&Default::default()), ..Default::default() }
            .resolve(r, &ctx);
        assert!(!res.targets.is_empty(), "fixture: the substitution must resolve");

        let (md, range) =
            super::Backend::path_substitution_hover(&doc, &nodes, r, &res, &play, &no_buffers(), &[], &no_cache(), None).expect("hover");
        assert!(md.contains("prod.yml"), "the target it reached: {md}");
        assert!(md.contains("Substituting"), "the section header: {md}");
        assert!(md.contains("env"), "the variable substituted: {md}");
        assert!(md.contains("prod"), "the value used: {md}");
        // The range covers the reference, so the hover box sits on the path.
        let line = text.lines().nth(range.start.line as usize).unwrap();
        assert!(line.contains("include_tasks"), "anchored on the reference line: {line}");

        // Nothing to substitute means no hover, rather than an empty box.
        let plain = refs.iter().find(|r| !r.value.contains("{{"));
        if let Some(pr) = plain {
            let pres = ansible_core::resolve::Resolver { literals: Some(&Default::default()), ..Default::default() }
            .resolve(pr, &ctx);
            assert!(
                super::Backend::path_substitution_hover(&doc, &nodes, pr, &pres, &play, &no_buffers(), &[], &no_cache(), None).is_none(),
                "a literal path has nothing to substitute"
            );
        }

        // A templated name with no reachable definition also declines — the value is what
        // the hover exists to show, so without one there is nothing to say.
        let t2 = "- hosts: all\n  tasks:\n    - include_tasks: \"tasks/{{ unknown_v }}.yml\"\n";
        std::fs::write(&play, t2).unwrap();
        let d2 = ansible_core::parse::Document::new(t2.to_string());
        let n2 = d2.parse().unwrap();
        let refs2 = ansible_core::references::extract(&n2).refs;
        let r2 = &refs2[0];
        let res2 = ansible_core::resolve::Resolver { literals: Some(&Default::default()), ..Default::default() }
            .resolve(r2, &ctx);
        assert!(super::Backend::path_substitution_hover(&d2, &n2, r2, &res2, &play, &no_buffers(), &[], &no_cache(), None).is_none());
    }

    /// T-029 box 4: a resolved module hovers one line of provenance — collection and
    /// origin — not the raw candidate paths. `ansible.builtin.debug` also has an action
    /// twin in core, so the documentation-only caveat must appear.
    #[test]
    fn hover_shows_module_provenance_not_paths() {
        if ansible_core::install::AnsibleInstall::detect(None).package_dir.is_none() {
            return; // ansible not on PATH
        }
        let path = std::path::Path::new("../../demo/tasks/modules.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = ansible_core::parse::Document::new(text);
        let nodes = doc.parse().unwrap();
        let ctx = ansible_core::workspace::FileContext::discover(&path);
        let refs = ansible_core::references::extract(&nodes).refs;
        let r = refs
            .iter()
            .find(|r| r.value == "ansible.builtin.debug")
            .expect("ref in demo");
        let res = ansible_core::resolve::Resolver { literals: Some(&Default::default()), ..Default::default() }
            .resolve(r, &ctx);
        if res.status != ansible_core::resolve::Status::Resolved {
            return; // install detected but builtins not resolvable in this layout
        }
        let md = super::reference_hover(r, &res, &ctx, false).expect("hover expected").render();
        assert!(plain(&md).contains("ansible.builtin"), "collection in: {md}");
        assert!(md.contains("the Ansible install"), "origin in: {md}");
        assert!(
            md.contains("runs on the controller (action plugin)"),
            "run location in the header: {md}"
        );
        assert!(md.contains("- module: ["), "labelled module link in: {md}");
        assert!(md.contains("- action plugin: ["), "labelled action link in: {md}");
        assert!(
            md.find("- action plugin:").unwrap() < md.find("- module:").unwrap(),
            "the file that runs is listed first: {md}"
        );
        assert!(
            md.contains("[…/ansible/plugins/action/debug.py]("),
            "install path cut to its site-packages tail, truncation marked: {md}"
        );
        assert!(!plain(&md).contains("Tried:"), "no path dump without the setting: {md}");

        // The setting appends the dump rather than replacing the provenance.
        let md = super::reference_hover(r, &res, &ctx, true).expect("hover expected").render();
        assert!(md.contains("- module: ["), "links kept with the setting: {md}");
        assert!(plain(&md).contains("Tried:"), "path dump with the setting: {md}");
    }

    /// T-073: a bare module with a same-name action plugin in a cfg `action_plugins` dir
    /// runs on the controller — the hover finds the legacy plugin and links it, not just
    /// the target-side module.
    #[test]
    fn hover_finds_cfg_dir_action_plugin_twin() {
        let path = std::path::Path::new("../../demo/tasks/action_plugins.yml")
            .canonicalize()
            .unwrap();
        let doc = ansible_core::parse::Document::new(
            "- hosts: all\n  tasks:\n    - stage_files:\n        dest: /srv\n".into(),
        );
        let nodes = doc.parse().unwrap();
        let ctx = ansible_core::workspace::FileContext::discover(&path);
        let refs = ansible_core::references::extract(&nodes).refs;
        let r = refs.iter().find(|r| r.value == "stage_files").expect("bare ref");
        let res = ansible_core::resolve::Resolver { literals: Some(&Default::default()), ..Default::default() }
            .resolve(r, &ctx);
        let md = super::reference_hover(r, &res, &ctx, false).expect("hover expected").render();
        let norm = md.replace('\\', "/");
        assert!(norm.contains("runs on the controller (action plugin)"), "controller label in: {md}");
        assert!(norm.contains("- action plugin:") && norm.contains("- module:"), "both files listed in: {md}");
        assert!(norm.contains("plugins/action/stage_files.py"), "cfg-dir plugin linked in: {md}");
    }

    /// T-086: a collection module with a same-name `plugins/action/` twin runs on the
    /// controller. The old lookup was a POSIX substring replace on a native path, so on
    /// Windows it found nothing and the hover claimed the target host.
    #[test]
    fn hover_finds_collection_action_plugin_twin() {
        let path = std::path::Path::new("../../demo/tasks/action_plugins.yml")
            .canonicalize()
            .unwrap();
        let doc = ansible_core::parse::Document::new(
            "- hosts: all\n  tasks:\n    - demo.charlie.beacon:\n        msg: hi\n".into(),
        );
        let nodes = doc.parse().unwrap();
        let ctx = ansible_core::workspace::FileContext::discover(&path);
        let refs = ansible_core::references::extract(&nodes).refs;
        let r = refs.iter().find(|r| r.value == "demo.charlie.beacon").expect("module ref");
        let res = ansible_core::resolve::Resolver { literals: Some(&Default::default()), ..Default::default() }
            .resolve(r, &ctx);
        let md = super::reference_hover(r, &res, &ctx, false).expect("hover expected").render();
        let norm = md.replace('\\', "/");
        assert!(norm.contains("runs on the controller (action plugin)"), "controller label in: {md}");
        assert!(norm.contains("- action plugin:") && norm.contains("- module:"), "both files listed in: {md}");
        assert!(norm.contains("charlie/plugins/action/beacon.py"), "collection twin linked in: {md}");
    }

    /// T-086, both layouts, native paths by construction. `plugin_twin` doesn't stat, so
    /// the paths need not exist.
    #[test]
    fn plugin_twin_walks_components() {
        use std::path::PathBuf;
        let coll: PathBuf =
            ["c", "ansible_collections", "demo", "charlie", "plugins"].iter().collect();
        assert_eq!(
            super::plugin_twin(&coll.join("modules").join("beacon.py"), false),
            Some(coll.join("action").join("beacon.py"))
        );
        assert_eq!(
            super::plugin_twin(&coll.join("action").join("beacon.py"), true),
            Some(coll.join("modules").join("beacon.py"))
        );
        // Core install layout: no `plugins/` above `modules/`.
        let core: PathBuf = ["sp", "ansible"].iter().collect();
        assert_eq!(
            super::plugin_twin(&core.join("modules").join("debug.py"), false),
            Some(core.join("plugins").join("action").join("debug.py"))
        );
    }

    /// A path with neither shape yields None — never the input echoed back, which the old
    /// no-match `str::replace` did, making the hover list the same file as both halves.
    #[test]
    fn plugin_twin_rejects_shapeless_paths() {
        use std::path::PathBuf;
        let odd: PathBuf = ["x", "library", "stage_files.py"].iter().collect();
        assert_eq!(super::plugin_twin(&odd, false), None);
        assert_eq!(super::plugin_twin(&odd, true), None);
        // An action winner outside plugins/action/ has no modules/ twin to invent.
        let stray: PathBuf = ["x", "action_plugins", "deploy_report.py"].iter().collect();
        assert_eq!(super::plugin_twin(&stray, true), None);
    }

    /// The hover for one module reference in the demo's network fixture.
    fn network_hover(module: &str) -> String {
        let path = std::path::Path::new("../../demo/tasks/network_modules.yml")
            .canonicalize()
            .unwrap();
        let doc = ansible_core::parse::Document::new(format!(
            "- hosts: all\n  tasks:\n    - {module}:\n        x: 1\n"
        ));
        let nodes = doc.parse().unwrap();
        let ctx = ansible_core::workspace::FileContext::discover(&path);
        let refs = ansible_core::references::extract(&nodes).refs;
        let r = refs.iter().find(|r| r.value == module).expect("module ref");
        let res = ansible_core::resolve::Resolver { literals: Some(&Default::default()), ..Default::default() }
            .resolve(r, &ctx);
        super::reference_hover(r, &res, &ctx, false)
            .expect("hover expected")
            .render()
            .replace('\\', "/")
    }

    /// T-072: a network module has no same-name twin — one action plugin per *platform*
    /// serves the whole `<prefix>_*` family — so the hover must find `ios.py` for an
    /// `ios_config:` task and say the task runs on the controller.
    #[test]
    fn hover_names_the_network_platform_plugin() {
        for module in ["cisco.ios.ios_config", "cisco.ios.ios_facts"] {
            let md = network_hover(module);
            assert!(
                md.contains("runs on the controller (action plugin)"),
                "controller label for {module} in: {md}"
            );
            assert!(
                md.contains("- platform action plugin: ["),
                "platform plugin listed and linked for {module} in: {md}"
            );
            assert!(
                md.contains("cisco/ios/plugins/action/ios.py"),
                "the platform plugin, not a same-name file, for {module} in: {md}"
            );
            assert!(
                plain(&md).contains("handled by the ios platform plugin"),
                "explains the name mismatch for {module} in: {md}"
            );
        }
    }

    /// The other half of `task_executor.py:961-962`'s AND. `demo.charlie.link_status` has a
    /// `link.py` sitting in `plugins/action/` exactly where `ios.py` sits for `ios_config` —
    /// but `link` is not a configured platform, so Ansible ignores it and the module ships
    /// to the target host. An implementation that only probes for `<prefix>.py` passes the
    /// two `ios_*` cases above and gets this one wrong. (T-072)
    #[test]
    fn a_same_prefix_plugin_is_not_a_platform_plugin() {
        let md = network_hover("demo.charlie.link_status");
        assert!(
            md.contains("runs on the target host"),
            "an unlisted prefix does not move the task to the controller: {md}"
        );
        assert!(
            !md.contains("platform action plugin"),
            "the decoy plugin must not be claimed: {md}"
        );
        assert!(
            !md.contains("plugins/action/link.py"),
            "and must not be linked: {md}"
        );
    }

    /// A tooltip with its inline markdown stripped, so an assertion can be about what the
    /// hover *says* rather than how it is punctuated (T-082). A test named for provenance
    /// should not fail because a label gained emphasis.
    ///
    /// Structural checks — that a path is *linked*, that a block is present at all — still
    /// read the raw markdown, because there the punctuation is the behaviour.
    fn plain(md: &str) -> String {
        md.replace("**", "").replace('`', "")
    }

    /// T-082: a `when:` carrying markdown syntax must reach the tooltip as the user wrote
    /// it. Both halves of the bug, in one fixture:
    ///
    /// - a **backtick** ends a naive `` format!("`{c}`") `` span early, so the rest of the
    ///   expression escapes its code formatting;
    /// - **asterisks** in a value that `classify` *does* label go in unfenced, and would
    ///   render as emphasis — silently deleting the characters from what is shown.
    ///
    /// Neither is exotic: unmatched conditions are echoed verbatim, which is the common
    /// case, and `regex_search` patterns are full of both.
    #[test]
    fn a_condition_carrying_markdown_survives_into_the_tooltip() {
        let hover = |when: &str| {
            let src = format!(
                "- hosts: all\n  tasks:\n    - include_tasks: x.yml\n      when: {when}\n"
            );
            let doc = ansible_core::parse::Document::new(src);
            let nodes = doc.parse().expect("fixture parses");
            let refs = ansible_core::references::extract(&nodes).refs;
            let r = refs
                .iter()
                .find(|r| !r.conditions.is_empty())
                .expect("conditional reference");
            super::when_hover(r)
        };

        // Unlabelled — the expression is echoed, so it needs a fence wider than its own
        // backticks. The whole expression must sit inside one span.
        let md = hover(r#"msg | regex_search("`a`*b*")"#);
        assert!(md.contains(r#"``msg | regex_search("`a`*b*")``"#), "fenced whole in: {md}");

        // Labelled — `classify` recognises the `| default(…)` shape and interpolates the
        // value into English, which goes in *unfenced*. There the asterisks must be
        // escaped rather than eaten by emphasis.
        let md = hover(r#"deploy | default('a') == '*prod*'"#);
        assert!(md.contains("runs only if deploy = "), "labelled in: {md}");
        assert!(md.contains(r"\*prod\*"), "asterisks escaped in: {md}");
        assert!(!md.contains("= *prod*"), "would render as emphasis in: {md}");
    }

    /// T-078 against the real demo: on a task carrying both a module and a `when:`, the
    /// three tokens hover three different things, and none of it moves with the
    /// inlay-hints setting — the module name used to be unreachable with hints on and the
    /// condition unreachable with them off.
    #[test]
    fn when_module_and_variable_each_own_their_token() {
        let path = std::path::Path::new("../../demo/tasks/variables.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = ansible_core::parse::Document::new(text.clone());
        let nodes = doc.parse().unwrap();

        // The task under test: `debug` + `msg: {{ cmd_result.stdout }}` + `when: cmd_result.rc == 0`.
        let when_kw = text.find("when: cmd_result.rc").unwrap();
        let module = text[..when_kw].rfind("ansible.builtin.debug").unwrap() + 4;
        let cond_var = when_kw + "when: ".len() + 2;

        for hints in [true, false] {
            let settings = Settings { hints, ..Settings::default() };
            let hover = |byte: usize| {
                super::hover_at(&doc, &nodes, &path, byte, settings, &no_buffers(), &[], &no_cache(), None)
                    .unwrap_or_else(|| panic!("hover expected at {byte} (hints={hints})"))
                    .0
            };

            let kw = hover(when_kw + 1);
            assert!(plain(&kw).contains("when:"), "condition explained on the keyword: {kw}");
            assert!(kw.contains("cmd_result.rc == 0"), "clause spelled out in: {kw}");

            // Whatever the module name hovers, it is the module's own hover and not the
            // condition's. (Which line `ansible.builtin.debug` produces depends on there
            // being an Ansible install to resolve into; that it isn't the `when:` text
            // does not — see the sibling test for the provenance half.)
            let m = hover(module);
            assert!(!plain(&m).contains("when:"), "condition must not claim the module token: {m}");

            // The condition's *value* belongs to the variables written in it, so hover
            // agrees with Cmd+click instead of restating the guard.
            let v = hover(cond_var);
            assert!(v.contains("cmd_result"), "registration shown in: {v}");
            assert!(!plain(&v).contains("when:"), "keyword hover must not leak onto its value: {v}");
        }
    }

    /// T-078, the provenance half: a module that resolves inside the workspace still shows
    /// its provenance when the task carries a `when:` and inlay hints are on — the
    /// combination that used to make the module hover unreachable.
    #[test]
    fn module_provenance_survives_a_when_with_hints_on() {
        let path = std::path::Path::new("../../demo/playbook.yml")
            .canonicalize()
            .unwrap();
        let text = "- hosts: all\n  tasks:\n    - ping:\n        data: x\n      when: feature_on\n";
        let doc = ansible_core::parse::Document::new(text.into());
        let nodes = doc.parse().unwrap();
        let settings = Settings { hints: true, ..Settings::default() };

        let md = super::hover_at(&doc, &nodes, &path, text.find("ping:").unwrap() + 1, settings, &no_buffers(), &[], &no_cache(), None)
            .expect("module hover expected")
            .0;
        assert!(md.contains("ansible.legacy"), "provenance, not the condition, in: {md}");

        // And the condition is still reachable — on its keyword.
        let kw = super::hover_at(&doc, &nodes, &path, text.find("when:").unwrap() + 1, settings, &no_buffers(), &[], &no_cache(), None)
            .expect("when hover expected")
            .0;
        assert!(plain(&kw).contains("when:"), "condition on its keyword in: {kw}");
    }

    /// T-073 contrast: a plain module with no action plugin in any legacy dir (nor an
    /// install twin) still reports the target host — the legacy lookup must not invent one.
    #[test]
    fn hover_plain_module_with_no_twin_runs_on_target() {
        let path = std::path::Path::new("../../demo/tasks/action_plugins.yml")
            .canonicalize()
            .unwrap();
        let doc = ansible_core::parse::Document::new(
            "- hosts: all\n  tasks:\n    - purge_cache:\n        path: /var/cache\n".into(),
        );
        let nodes = doc.parse().unwrap();
        let ctx = ansible_core::workspace::FileContext::discover(&path);
        let refs = ansible_core::references::extract(&nodes).refs;
        let r = refs.iter().find(|r| r.value == "purge_cache").expect("bare ref");
        let res = ansible_core::resolve::Resolver { literals: Some(&Default::default()), ..Default::default() }
            .resolve(r, &ctx);
        let md = super::reference_hover(r, &res, &ctx, false).expect("hover expected").render();
        assert!(md.contains("runs on the target host"), "target label in: {md}");
        assert!(!md.contains("action plugin"), "no phantom action plugin in: {md}");
    }

    /// T-073: a role's own `action_plugins/` dir overrides a same-name module for tasks in
    /// that role — the hover finds the role-local plugin and reports the controller.
    #[test]
    fn hover_finds_role_local_action_plugin_twin() {
        let path = std::path::Path::new("../../demo/roles/reporting/tasks/main.yml")
            .canonicalize()
            .unwrap();
        let doc = ansible_core::parse::Document::new("- deploy_report:\n    summary: x\n".into());
        let nodes = doc.parse().unwrap();
        let ctx = ansible_core::workspace::FileContext::discover(&path);
        let refs = ansible_core::references::extract(&nodes).refs;
        let r = refs.iter().find(|r| r.value == "deploy_report").expect("bare ref");
        let res = ansible_core::resolve::Resolver { literals: Some(&Default::default()), ..Default::default() }
            .resolve(r, &ctx);
        let md = super::reference_hover(r, &res, &ctx, false).expect("hover expected").render();
        let norm = md.replace('\\', "/");
        assert!(norm.contains("runs on the controller (action plugin)"), "controller label in: {md}");
        assert!(norm.contains("action_plugins/deploy_report.py"), "role-local plugin linked in: {md}");
    }

    /// A bare name that wins from a workspace `library/` dir is labelled `ansible.legacy`
    /// (Ansible's own name for the pre-collections namespace), not builtin.
    #[test]
    fn hover_labels_workspace_library_modules_legacy() {
        let path = std::path::Path::new("../../demo/playbook.yml")
            .canonicalize()
            .unwrap();
        let doc = ansible_core::parse::Document::new(
            "- hosts: all\n  tasks:\n    - ping:\n        data: x\n".into(),
        );
        let nodes = doc.parse().unwrap();
        let ctx = ansible_core::workspace::FileContext::discover(&path);
        let refs = ansible_core::references::extract(&nodes).refs;
        let r = refs.iter().find(|r| r.value == "ping").expect("bare ref");
        let res = ansible_core::resolve::Resolver { literals: Some(&Default::default()), ..Default::default() }
            .resolve(r, &ctx);
        assert_eq!(res.status, ansible_core::resolve::Status::Resolved);
        let md = super::reference_hover(r, &res, &ctx, false).expect("hover expected").render();
        assert!(plain(&md).contains("ansible.legacy"), "legacy label in: {md}");
        assert!(md.contains("library/ping.py]("), "library path linked in: {md}");
    }

    /// A bare name that resolved into a collection tree got there via the 2.10 split
    /// table — the hover must show that hop, or the Tried list has an unmarked seam.
    #[test]
    fn hover_marks_the_split_table_redirect() {
        let path = std::path::Path::new("../../demo/playbook.yml")
            .canonicalize()
            .unwrap();
        let doc = ansible_core::parse::Document::new(
            "- hosts: all\n  tasks:\n    - docker_container:\n        name: x\n".into(),
        );
        let nodes = doc.parse().unwrap();
        let ctx = ansible_core::workspace::FileContext::discover(&path);
        let refs = ansible_core::references::extract(&nodes).refs;
        let r = refs.iter().find(|r| r.value == "docker_container").expect("bare ref");
        let res = ansible_core::resolve::Resolver { literals: Some(&Default::default()), ..Default::default() }
            .resolve(r, &ctx);
        if res.status != ansible_core::resolve::Status::Resolved {
            return; // community.docker not installed here — nothing to mark
        }
        let md = super::reference_hover(r, &res, &ctx, false).expect("hover expected").render();
        assert!(
            plain(&md).contains("redirected to community.docker.docker_container"),
            "redirect hop marked in: {md}"
        );
    }

    /// A missing or malformed key must keep the default. Turning a feature off because a
    /// client sent an unexpected shape would look like the feature is broken.
    /// One switch, and anything unexpected keeps hints ON. Silently disabling a
    /// feature because a client sent an odd shape is indistinguishable from a bug.
    #[test]
    fn hints_default_on_and_only_an_explicit_false_disables_them() {
        assert!(Settings::from_json(&serde_json::json!({})).hints);
        assert!(!Settings::from_json(&serde_json::json!({
            "inlayHints": { "enabled": false }
        }))
        .hints);

        for junk in [
            serde_json::json!({ "inlayHints": { "enabled": "no" } }),
            serde_json::json!({ "unrelated": true }),
            // The two retired keys. Both once controlled a tooltip; that tooltip is
            // gone, and a stale setting must not turn the hints off by accident.
            serde_json::json!({ "inlayHints": { "explanations": false } }),
            serde_json::json!({ "inlayHints": { "tooltips": false } }),
        ] {
            assert!(
                Settings::from_json(&junk).hints,
                "should not disable hints: {junk}"
            );
        }
    }

    /// T-178's per-consumer box, at the one surface measured to show the key.
    ///
    /// Hover and go-to-definition answer from the same index since T-224 (a definition
    /// first, the injected table only after), so they now agree with this surface; the
    /// templated-path hover is the one that was measured to show the key when this was
    /// written, and stays the pinned consumer.
    ///
    /// Each fixture carries an ordinary `control` variable in the SAME position, used in a
    /// second templated path. That is what keeps the negative honest: the group case asserts
    /// an absence, and an absence also appears when the inventory was never read at all —
    /// `cached_definitions` builds `ScanCache::default()` with no `with_env`, so an ambient
    /// `ANSIBLE_CONFIG` can replace the fixture's `ansible.cfg`. If that happens the control
    /// path stops substituting and this test fails loudly instead of passing empty.
    #[test]
    fn group_priority_substitutes_a_path_from_a_host_position_and_never_from_a_group() {
        use ansible_core::testing::project;

        const PLAY: &str = concat!(
            "- hosts: all\n",
            "  vars_files:\n",
            "    - \"vars/{{ ansible_group_priority }}.yml\"\n",
            "    - \"vars/{{ control }}.yml\"\n",
        );

        // Both fixtures resolve to the same target file, so the only difference between them
        // is the inventory position the value came from.
        let group = project(
            "t178-path-group",
            "[defaults]\ninventory = inv.ini\n",
            &[
                ("inv.ini", "[web]\nnode1\n\n[web:vars]\nansible_group_priority=10\ncontrol=10\n"),
                ("vars/10.yml", "x: 1\n"),
                ("play.yml", PLAY),
            ],
        );
        let host = project(
            "t178-path-host",
            "[defaults]\ninventory = inv.ini\n",
            &[
                ("inv.ini", "[web]\nnode1 ansible_group_priority=10 control=10\n"),
                ("vars/10.yml", "x: 1\n"),
                ("play.yml", PLAY),
            ],
        );

        let hover_on = |root: &std::path::Path, name: &str| -> String {
            let path = root.join("play.yml");
            let doc = super::Document::new(PLAY.to_string());
            let nodes = doc.parse().expect("fixture parses");
            let needle = format!("vars/{{{{ {name} }}}}.yml");
            let byte = PLAY.find(&needle).expect("fixture carries the path") + 1;
            super::hover_at(&doc, &nodes, &path, byte, Default::default(), &no_buffers(), &[], &no_cache(), None)
                .map(|h| h.0)
                .unwrap_or_default()
        };

        // Host position: the key is a real variable there, so the path substitutes with it
        // and the hover sources the value to the inventory line.
        let from_host = hover_on(&host, "ansible_group_priority");
        assert!(
            from_host.contains("`ansible_group_priority` = `10`"),
            "host position must substitute:\n{from_host}"
        );
        assert!(
            from_host.contains("inv.ini"),
            "and name the inventory as the source:\n{from_host}"
        );

        // Group position: ansible consumes the key as merge order and never defines it, so
        // there is nothing to substitute and the hover must not claim a value.
        let from_group = hover_on(&group, "ansible_group_priority");
        assert!(
            !from_group.contains("`ansible_group_priority` = "),
            "group position must not substitute:\n{from_group}"
        );

        // The control, in the same group `[web:vars]` section: an ordinary name there IS a
        // variable, and must still substitute. Without this the assertion above would also
        // pass on a fixture whose inventory was never read.
        let control = hover_on(&group, "control");
        assert!(
            control.contains("`control` = `10`") && control.contains("inv.ini"),
            "the group vars section was read, and only the priority key was dropped:\n{control}"
        );
    }

    // ---- T-199: an unsaved buffer in a *second* file -------------------------------------
    //
    // `shared_port` is defined in `vars/x.yml` and used from `play.yml`, so every answer
    // about it has to read a file other than the one the cursor is in. That is the whole
    // bug: the current file's text comes from the buffer, every other file comes off disk.

    const T199_PLAY: &str = concat!(
        "- hosts: all\n",
        "  vars_files:\n",
        "    - vars/x.yml\n",
        "  tasks:\n",
        "    - name: use it\n",
        "      ansible.builtin.debug:\n",
        "        msg: \"{{ shared_port }}\"\n",
    );

    const T199_SAVED: &str = "shared_port: 8080\n";

    /// The same key edited and not saved: a different value, ten lines further down. The
    /// shift is deliberate — a wrong *value* is a wrong hover, but a wrong *line* is what
    /// makes the jump land somewhere the editor is not drawing the definition.
    const T199_DIRTY: &str = concat!(
        "# 1\n# 2\n# 3\n# 4\n# 5\n# 6\n# 7\n# 8\n# 9\n# 10\n",
        "shared_port: 9999\n",
    );

    fn t199_project(name: &str) -> std::path::PathBuf {
        ansible_core::testing::project(
            name,
            "[defaults]\n",
            &[("vars/x.yml", T199_SAVED), ("play.yml", T199_PLAY)],
        )
    }

    /// A live server over one fixture project, driven through the real notification handlers.
    ///
    /// The earlier version of these tests called a helper that did `invalidate_var_cache` and
    /// built an `OpenDocs` by hand — i.e. it performed `did_open`/`did_change`'s work itself.
    /// Measured: deleting the invalidation from all three handlers left the whole suite green,
    /// so T-199's actual fix was pinned by nothing. Driving the handlers is the difference
    /// between testing the server and testing a helper that imitates it.
    struct T199Server {
        service: tower_lsp::LspService<super::Backend>,
        root: std::path::PathBuf,
    }

    impl T199Server {
        fn new(name: &str) -> Self {
            let root = t199_project(name);
            let state = scan_state(&root);
            Self { service: lsp_service(state), root }
        }

        fn backend(&self) -> &super::Backend {
            self.service.inner()
        }

        fn uri(&self, rel: &str) -> tower_lsp::lsp_types::Url {
            tower_lsp::lsp_types::Url::from_file_path(self.root.join(rel)).unwrap()
        }

        /// `textDocument/didOpen`, as the editor sends it.
        async fn open(&self, rel: &str, text: &str) {
            use tower_lsp::LanguageServer;
            self.backend()
                .did_open(tower_lsp::lsp_types::DidOpenTextDocumentParams {
                    text_document: tower_lsp::lsp_types::TextDocumentItem {
                        uri: self.uri(rel),
                        language_id: "ansible".into(),
                        version: 1,
                        text: text.to_string(),
                    },
                })
                .await;
        }

        /// `textDocument/didChange` with FULL sync — an unsaved edit. This is the notification
        /// whose invalidation the three tests below actually depend on.
        async fn change(&self, rel: &str, text: &str) {
            use tower_lsp::LanguageServer;
            self.backend()
                .did_change(tower_lsp::lsp_types::DidChangeTextDocumentParams {
                    text_document: tower_lsp::lsp_types::VersionedTextDocumentIdentifier {
                        uri: self.uri(rel),
                        version: 2,
                    },
                    content_changes: vec![tower_lsp::lsp_types::TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text: text.to_string(),
                    }],
                })
                .await;
        }

        fn use_position(&self) -> tower_lsp::lsp_types::Position {
            let doc = super::Document::new(T199_PLAY.to_string());
            let byte = T199_PLAY.find("{{ shared_port }}").expect("fixture carries the use") + 3;
            let (l, c) = doc.byte_to_lsp(byte);
            tower_lsp::lsp_types::Position::new(l, c)
        }

        /// `textDocument/hover` on the cross-file use, through the handler.
        async fn hover(&self) -> String {
            use tower_lsp::LanguageServer;
            let p = tower_lsp::lsp_types::HoverParams {
                text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
                    text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                        uri: self.uri("play.yml"),
                    },
                    position: self.use_position(),
                },
                work_done_progress_params: Default::default(),
            };
            match self.backend().hover(p).await.expect("hover does not error") {
                Some(h) => match h.contents {
                    tower_lsp::lsp_types::HoverContents::Markup(m) => m.value,
                    other => panic!("unexpected hover shape: {other:?}"),
                },
                None => String::new(),
            }
        }

        /// `textDocument/definition` on the same use, through the handler.
        async fn definition(&self) -> tower_lsp::lsp_types::Location {
            use tower_lsp::LanguageServer;
            let p = tower_lsp::lsp_types::GotoDefinitionParams {
                text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
                    text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                        uri: self.uri("play.yml"),
                    },
                    position: self.use_position(),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            };
            match self.backend().goto_definition(p).await.expect("definition does not error") {
                Some(tower_lsp::lsp_types::GotoDefinitionResponse::Scalar(l)) => l,
                Some(tower_lsp::lsp_types::GotoDefinitionResponse::Array(mut v)) => {
                    assert_eq!(v.len(), 1, "the use has one effective definition");
                    v.remove(0)
                }
                other => panic!("expected a definition, got {other:?}"),
            }
        }
    }

    /// T-199, consumer 1 of 2 (rule 3): the hover states a *value*, so reading disk while the
    /// screen shows something else is a confident wrong number.
    #[tokio::test]
    async fn hover_of_a_cross_file_use_reads_the_open_buffer_not_the_saved_file() {
        let s = T199Server::new("t199-hover");
        s.open("play.yml", T199_PLAY).await;

        // Control, with `vars/x.yml` never opened: disk is the only text there is, and the
        // answer must come from it. Without this the assertions below would also pass on a
        // build that read a buffer unconditionally — including for files nobody opened.
        let saved = s.hover().await;
        assert!(saved.contains("= `8080`"), "a closed file answers from disk:\n{saved}");
        assert!(saved.contains("x.yml:1"), "and from its line on disk:\n{saved}");

        // Now open it and edit it without saving — through the real notifications, so the
        // invalidation is the server's rather than the test's.
        s.open("vars/x.yml", T199_SAVED).await;
        s.change("vars/x.yml", T199_DIRTY).await;

        let dirty = s.hover().await;
        assert!(dirty.contains("= `9999`"), "hover must state what is on screen:\n{dirty}");
        assert!(!dirty.contains("8080"), "and never the saved value:\n{dirty}");
        assert!(dirty.contains("x.yml:11"), "sourced to the buffer's line:\n{dirty}");
    }

    /// T-199, consumer 2 of 2: go-to-definition returns a *range*, which the editor applies
    /// to the text it is drawing. Computed against disk and applied to a dirty buffer, the
    /// two disagree by however far the unsaved edit shifted the definition.
    #[tokio::test]
    async fn go_to_definition_returns_a_range_valid_against_the_open_buffer() {
        let s = T199Server::new("t199-goto");
        s.open("play.yml", T199_PLAY).await;

        let saved = s.definition().await;
        assert_eq!(
            saved.range.start.line, 0,
            "control: with nothing open the definition is on disk line 0"
        );

        s.open("vars/x.yml", T199_SAVED).await;
        s.change("vars/x.yml", T199_DIRTY).await;

        let dirty = s.definition().await;
        assert_eq!(dirty.uri, s.uri("vars/x.yml"));
        assert_eq!(
            dirty.range.start.line, 10,
            "the editor draws the buffer, so the range has to be the buffer's"
        );
    }

    /// The overlay must be inert where it has nothing to add: a file open but untouched
    /// carries the same text as disk, so both consumers must answer identically to the
    /// closed case. This is what stops "prefer the buffer" from becoming a second code path
    /// with its own answers.
    #[tokio::test]
    async fn an_open_but_unedited_buffer_answers_exactly_like_the_saved_file() {
        let s = T199Server::new("t199-clean");
        s.open("play.yml", T199_PLAY).await;

        let closed_hover = s.hover().await;
        let closed_def = s.definition().await;

        // Opened, and left exactly as it is on disk.
        s.open("vars/x.yml", T199_SAVED).await;

        assert_eq!(s.hover().await, closed_hover, "hover unchanged by an unedited buffer");
        assert_eq!(s.definition().await, closed_def, "and so is the jump target");
    }

    // ---- T-201: the inventory setting is a per-request value, not a process slot ---------
    //
    // One project, two inventory files. The *only* thing that differs between the two
    // requests below is which one `ansibleLsp.inventory` names, so a difference in the answer
    // can only have come from the setting.

    const T201_PLAY: &str = "- hosts: all\n  vars_files:\n    - \"vars/{{ control }}.yml\"\n";

    fn t201_project(name: &str) -> std::path::PathBuf {
        ansible_core::testing::project(
            name,
            "[defaults]\n",
            &[
                ("inv_a.ini", "[web]\nnode1 control=11\n"),
                ("inv_b.ini", "[web]\nnode1 control=22\n"),
                ("vars/11.yml", "x: 1\n"),
                ("vars/22.yml", "x: 1\n"),
                ("play.yml", T201_PLAY),
            ],
        )
    }

    /// A `State` carrying one workspace root and one `ansibleLsp.inventory` value.
    fn t201_state(root: &std::path::Path, inv: &str) -> super::State {
        let state = super::State {
            docs: Default::default(),
            roots: std::sync::Mutex::new(vec![root.to_path_buf()]),
            flagged: Default::default(),
            render_sites: Default::default(),
            template_grammars: Default::default(),
            mutations: Default::default(),
            settings: Default::default(),
            inventory: Default::default(),
            startup_note: Default::default(),
            scanning: std::sync::atomic::AtomicBool::new(false),
            var_cache: no_cache(),
            scan_task: Default::default(),
            ansible_path: Default::default(),
            install: Default::default(),
        };
        state.set_inventory(&serde_json::json!({ "inventory": [inv] }));
        state
    }

    /// The templated-path hover, which substitutes `{{ control }}` from the inventory the
    /// request was given. Chosen because it *names its source*, so a wrong answer is visible
    /// as a wrong file rather than only as a wrong number.
    fn t201_hover(
        root: &std::path::Path,
        inv: &[std::path::PathBuf],
        cache: &std::sync::Mutex<super::VarCache>,
    ) -> String {
        let path = root.join("play.yml");
        let doc = super::Document::new(T201_PLAY.to_string());
        let nodes = doc.parse().expect("fixture parses");
        let byte = T201_PLAY.find("vars/{{ control }}.yml").expect("fixture carries the path") + 1;
        super::hover_at(&doc, &nodes, &path, byte, Default::default(), &no_buffers(), inv, cache, None)
            .map(|h| h.0)
            .unwrap_or_default()
    }

    /// Each request answers from the inventory *it* was given.
    ///
    /// The control is that the two answers differ. An assertion that only checked "A says 11"
    /// would also pass if the setting were ignored entirely and both requests fell through to
    /// the fixture's `ansible.cfg`, which names no inventory at all — that case substitutes
    /// nothing, so both `contains` checks would fail rather than pass empty.
    #[test]
    fn each_request_answers_from_the_inventory_it_was_given() {
        let root = t201_project("t201-each-request");
        let a = t201_state(&root, "inv_a.ini");
        let b = t201_state(&root, "inv_b.ini");
        // One cache for both requests. That is the case worth pinning: keyed by path alone it
        // handed request B the answer it had already computed for request A.
        let cache = no_cache();

        let from_a = t201_hover(&root, &a.inventory_setting(), &cache);
        assert!(
            from_a.contains("`control` = `11`") && from_a.contains("inv_a.ini"),
            "request A must answer from inv_a.ini:\n{from_a}"
        );

        let from_b = t201_hover(&root, &b.inventory_setting(), &cache);
        assert!(
            from_b.contains("`control` = `22`") && from_b.contains("inv_b.ini"),
            "request B must answer from inv_b.ini:\n{from_b}"
        );
    }

    /// The bug itself: a *later* writer must not change an answer already in flight.
    ///
    /// This is the shape that made `group_priority_substitutes_…` fail intermittently — one
    /// test wrote the global while another was reading through it. Here the write happens
    /// between snapshot and use, which is the same window made deterministic.
    #[test]
    fn a_later_writer_cannot_change_a_snapshot_already_taken() {
        let root = t201_project("t201-later-writer");
        let a = t201_state(&root, "inv_a.ini");

        // Request A takes its snapshot...
        let snapshot = a.inventory_setting();

        // ...and only then does a second request set a different inventory. Under the process
        // global this write landed in the slot request A was about to read.
        let b = t201_state(&root, "inv_b.ini");
        let _ = b.inventory_setting();

        let from_a = t201_hover(&root, &snapshot, &no_cache());
        assert!(
            from_a.contains("`control` = `11`") && from_a.contains("inv_a.ini"),
            "the in-flight request keeps its own inventory:\n{from_a}"
        );
    }

    /// Setting an inventory on one `State` must not reach another. The globals were shadow
    /// copies of these two fields, so this is the invariant that used to be violated.
    #[test]
    fn one_states_inventory_setting_is_invisible_to_another() {
        let root = t201_project("t201-invisible");
        let a = t201_state(&root, "inv_a.ini");
        let b = t201_state(&root, "inv_b.ini");

        assert_eq!(a.inventory_setting(), vec![root.join("inv_a.ini")]);
        assert_eq!(b.inventory_setting(), vec![root.join("inv_b.ini")]);

        // And a third with no setting at all falls through to the config, as an editor with
        // nothing configured must.
        let none = t201_state(&root, "");
        assert!(none.inventory_setting().is_empty());
    }

    // ---- T-201 box (3): the scan writes into the server's cache, not one of its own -------
    //
    // The only test here that needs a real `Client`. There is no public constructor for one —
    // the closure passed to `LspService::new` is the sole source, which is what `main` uses —
    // so standing a service up is the only way to reach `scan_workspace` at all.
    //
    // The socket is dropped rather than drained. `Client`'s send is
    // `if tx.send(req).await.is_err() { return Err(ExitedError(())) }` (tower-lsp 0.20,
    // `service/client.rs:549`), so with the receiver gone every publish fails instantly and is
    // swallowed. Holding the socket without polling it is what would deadlock — the channel is
    // `mpsc::channel(1)` — so dropping it is deliberate, not laziness.
    //
    // The cache is the observable, not the notifications: this pins the one line
    // (`&st.var_cache` at the spawn) that makes the scan share the server's index instead of
    // building a private one. Nothing else in the suite can see that line.

    /// Through the real `did_open`, because "the pure function returns a diagnostic" and
    /// "opening the file produces one" are different claims, and only the second is what a
    /// user gets. The routing under test is `publish_diagnostics` taking the template branch:
    /// a `.j2` is not YAML, so without it every template in a repo would come back
    /// `unparseable` — true about the bytes, a lie about the file.
    #[tokio::test]
    async fn opening_a_broken_template_flags_it_and_a_good_one_does_not() {
        use tower_lsp::LanguageServer;
        let root = ansible_core::testing::project(
            "j2-did-open",
            "[defaults]
",
            &[
                ("templates/bad.j2", "{% forr x in xs %}{% endforr %}
"),
                ("templates/good.j2", "{% for x in xs %}{{ x }}{% endfor %}
"),
            ],
        );
        let state = scan_state(&root);
        let service = lsp_service(state.clone());
        let open = |rel: &str, text: &str| {
            let uri = tower_lsp::lsp_types::Url::from_file_path(root.join(rel)).unwrap();
            let text = text.to_string();
            async {
                service
                    .inner()
                    .did_open(tower_lsp::lsp_types::DidOpenTextDocumentParams {
                        text_document: tower_lsp::lsp_types::TextDocumentItem {
                            uri: uri.clone(),
                            language_id: "jinja".into(),
                            version: 1,
                            text,
                        },
                    })
                    .await;
                uri
            }
        };
        let bad = open("templates/bad.j2", "{% forr x in xs %}{% endforr %}
").await;
        let good = open("templates/good.j2", "{% for x in xs %}{{ x }}{% endfor %}
").await;
        let flagged = state.flagged.lock().unwrap().clone();
        assert!(flagged.contains(&bad), "the broken template was not flagged");
        // The control that matters: a template is never reported merely for not being YAML.
        assert!(!flagged.contains(&good), "a template that renders was flagged anyway");
    }

    /// A false "will not render" on two real templates in `~/app/ansible`, which wrap a script
    /// body in `{% raw %}` and print a `{%s}` format inside it. jinja2 renders both; we put a
    /// `template-syntax` ERROR on them. Asserted through `did_open` because the surface that
    /// matters is the squiggle, not the reader's return value.
    #[tokio::test]
    async fn a_stray_open_inside_a_raw_body_is_text_not_an_unterminated_raw() {
        use tower_lsp::LanguageServer;
        let body = "{% raw %}\nplain {%s} text\n{% endraw %}\n";
        let root = ansible_core::testing::project(
            "j2-raw-probe",
            "[defaults]\n",
            &[("templates/raw.j2", body)],
        );
        let state = scan_state(&root);
        let service = lsp_service(state.clone());
        let uri = tower_lsp::lsp_types::Url::from_file_path(root.join("templates/raw.j2")).unwrap();
        service
            .inner()
            .did_open(tower_lsp::lsp_types::DidOpenTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "jinja".into(),
                    version: 1,
                    text: body.to_string(),
                },
            })
            .await;
        let flagged = state.flagged.lock().unwrap().clone();
        assert!(!flagged.contains(&uri), "jinja2 renders this, but we flagged it");
    }

    /// Decode the wire format back to absolute `(line, col, len, type-name)`, because the
    /// encoding is deltas and an assertion against raw deltas is unreadable — and would pass
    /// just as happily on a wrong absolute position reached by two cancelling mistakes.
    fn decoded(text: &str) -> Vec<(u32, u32, u32, &'static str)> {
        let d = ansible_core::jinja::Delimiters::default();
        let (mut line, mut col) = (0u32, 0u32);
        super::Backend::semantic_tokens_of(text, &d, true)
            .into_iter()
            .map(|t| {
                line += t.delta_line;
                col = if t.delta_line == 0 { col + t.delta_start } else { t.delta_start };
                let name = super::SEMANTIC_TOKEN_LEGEND[t.token_type as usize].as_str();
                (line, col, t.length, name)
            })
            .collect()
    }

    /// `legend_index` is a hand-kept mirror of `SEMANTIC_TOKEN_LEGEND`, and a wrong index
    /// paints the wrong colour without an error. Decoding through the legend by name is the
    /// only thing that catches the two drifting apart.
    #[test]
    fn a_property_token_decodes_to_the_legend_entry_named_property() {
        let got = decoded("{{ a.b }}");
        assert!(got.contains(&(0, 3, 1, "variable")), "{got:?}");
        assert!(got.contains(&(0, 5, 1, "property")), "{got:?}");
        let got = decoded("{% macro f(p) %}{% import 'x' as n %}");
        assert!(got.contains(&(0, 11, 1, "parameter")), "{got:?}");
        assert!(got.contains(&(0, 33, 1, "namespace")), "{got:?}");
        let got = decoded("{% block b %}");
        assert!(got.contains(&(0, 9, 1, "label")), "{got:?}");
    }

    /// The modifier travels as a bit whose position is the index in `SEMANTIC_TOKEN_MODIFIERS`,
    /// and a wrong bit paints nothing and errors nowhere. Decoded by name for the same reason
    /// as the types above.
    #[test]
    fn a_loop_target_carries_the_declaration_bit_and_the_iterable_does_not() {
        let d = ansible_core::jinja::Delimiters::default();
        let decl = super::SEMANTIC_TOKEN_MODIFIERS
            .iter()
            .position(|m| *m == tower_lsp::lsp_types::SemanticTokenModifier::DECLARATION)
            .expect("declaration is in the legend");
        let var = super::legend_index(ansible_core::jinja::TokenType::Variable);
        let got: Vec<(u32, bool)> = super::Backend::semantic_tokens_of("{% for h in hosts %}", &d, true)
            .into_iter()
            .filter(|t| t.token_type == var)
            .map(|t| (t.length, t.token_modifiers_bitset & (1 << decl) != 0))
            .collect();
        assert_eq!(got, [(1, true), (5, false)], "{got:?}");
    }

    /// The second modifier, by the same route: `hostvars` and `loop` carry `defaultLibrary`,
    /// the loop target and the user's own name do not (T-217).
    #[test]
    fn a_provided_name_carries_the_default_library_bit_and_a_users_name_does_not() {
        let d = ansible_core::jinja::Delimiters::default();
        let lib = super::SEMANTIC_TOKEN_MODIFIERS
            .iter()
            .position(|m| *m == tower_lsp::lsp_types::SemanticTokenModifier::DEFAULT_LIBRARY)
            .expect("defaultLibrary is in the legend");
        let var = super::legend_index(ansible_core::jinja::TokenType::Variable);
        let got: Vec<(u32, bool)> =
            super::Backend::semantic_tokens_of("{% for h in hostvars %}{{ loop.index }}{{ app }}{% endfor %}", &d, true)
                .into_iter()
                .filter(|t| t.token_type == var)
                .map(|t| (t.length, t.token_modifiers_bitset & (1 << lib) != 0))
                .collect();
        assert_eq!(got, [(1, false), (8, true), (4, true), (3, false)], "{got:?}");
    }

    /// The encoding is deltas against the previous token, and the column delta is absolute
    /// again whenever the line advances. Getting that reset wrong shifts every token after
    /// the first newline and nothing errors.
    #[test]
    fn tokens_encode_as_deltas_that_decode_back_to_the_right_places() {
        let got = decoded("{{ a }}\n{{ bb }}\n");
        assert_eq!(
            got,
            [
                (0, 0, 2, "delimiter"),
                (0, 3, 1, "variable"),
                (0, 5, 2, "delimiter"),
                // The line advanced, so this column is absolute again rather than a delta.
                (1, 0, 2, "delimiter"),
                (1, 3, 2, "variable"),
                (1, 6, 2, "delimiter"),
            ],
            "{got:?}"
        );
        // Two on one line: each is a delta from the previous, not from the line start.
        let same = decoded("{{ a }}{{ bb }}");
        assert_eq!(
            same,
            [
                (0, 0, 2, "delimiter"),
                (0, 3, 1, "variable"),
                (0, 5, 2, "delimiter"),
                (0, 7, 2, "delimiter"),
                (0, 10, 2, "variable"),
                (0, 13, 2, "delimiter"),
            ],
            "{same:?}"
        );
    }

    /// A token may not span lines, and a `{# … #}` comment routinely does. Split per line, or
    /// the client paints to the end of the first line and silently drops the rest.
    #[test]
    fn a_multiline_comment_is_split_into_one_token_per_line() {
        let got = decoded("{# one\ntwo\nthree #}");
        assert_eq!(got.len(), 3, "a comment over 3 lines must be 3 tokens: {got:?}");
        assert!(got.iter().all(|t| t.3 == "comment"), "{got:?}");
        assert_eq!(got.iter().map(|t| t.0).collect::<Vec<_>>(), [0, 1, 2], "{got:?}");
    }

    /// The case the client's grammar gets exactly backwards, asserted here at the wire so it
    /// is the editor's answer and not just the reader's — the tokens move to the real tags.
    #[test]
    fn an_overridden_delimiter_is_answered_from_the_header_not_the_default_pair() {
        let src = "#jinja2: block_start_string:'<%', block_end_string:'%>'\n<% if x %>\nand {% no %}\n";
        let got = decoded(src);
        assert!(got.iter().any(|t| t.3 == "keyword"), "no tag found at all: {got:?}");
        // Line 2 is literal text under this header, so nothing on it is a token.
        assert!(!got.iter().any(|t| t.0 == 2), "painted the literal braces: {got:?}");
    }

    /// A cold grammar cache must not block the answer. Building the map walks every YAML file
    /// and every template; blocking on it left a reloaded window showing plain text for ~10s,
    /// which is what the delay looked like in the editor.
    ///
    /// The control is the second call: once the map is warm the overridden delimiters are
    /// honoured, so "answers immediately" did not become "answers wrongly forever".
    #[tokio::test]
    async fn a_cold_grammar_cache_answers_at_once_and_the_warm_one_honours_the_header() {
        use tower_lsp::LanguageServer;
        // No `#jinja2:` header on purpose: a file that declares its own delimiters is read
        // from the header and never needs the map. These come from the *task*, which is the
        // only case the grammar map answers — and the only one a cold cache can get wrong.
        let src = "<% if x %>\n";
        let root = ansible_core::testing::project(
            "j2-cold-cache",
            "[defaults]\n",
            &[
                ("templates/h.j2", src),
                (
                    "play.yml",
                    "- hosts: all\n  tasks:\n    - ansible.builtin.template:\n        \
                     src: h.j2\n        dest: /tmp/h\n        block_start_string: \"<%\"\n\
                     \x20       block_end_string: \"%>\"\n",
                ),
            ],
        );
        let state = scan_state(&root);
        let service = lsp_service(state.clone());
        let uri = tower_lsp::lsp_types::Url::from_file_path(root.join("templates/h.j2")).unwrap();
        service
            .inner()
            .did_open(tower_lsp::lsp_types::DidOpenTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "jinja".into(),
                    version: 1,
                    text: src.to_string(),
                },
            })
            .await;
        let ask = |u: tower_lsp::lsp_types::Url| {
            let s = &service;
            async move {
                s.inner()
                    .semantic_tokens_full(tower_lsp::lsp_types::SemanticTokensParams {
                        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: u },
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                    })
                    .await
                    .unwrap()
            }
        };
        // Cold on purpose, and cleared *here*: `did_open` publishes diagnostics, and that path
        // builds the same map, so clearing any earlier is not a cold cache by the time we ask.
        state.template_grammars.lock().unwrap().clear();

        // Cold: answered from the default delimiters, so `<% if x %>` is NOT read as a tag.
        // That is the discriminating assertion — the blocking version built the map inline and
        // would have honoured the header here, so this fails if the answer ever waits again.
        // A timing assertion could not do this job: on a one-file fixture both versions are
        // fast, and the probe would pass without being able to fail.
        let cold = ask(uri.clone()).await;
        let cold = match cold {
            Some(tower_lsp::lsp_types::SemanticTokensResult::Tokens(t)) => t.data,
            other => panic!("a cold cache must still answer: {other:?}"),
        };
        let kw = super::SEMANTIC_TOKEN_LEGEND
            .iter()
            .position(|t| *t == tower_lsp::lsp_types::SemanticTokenType::KEYWORD);
        assert!(
            !cold.iter().any(|t| Some(t.token_type as usize) == kw),
            "the cold answer waited for the grammar instead of using the defaults: {cold:?}"
        );

        // Warm: the header is honoured, so answering early did not become answering wrongly.
        super::Backend::template_grammar_cached(
            &state,
            &root.join("templates/h.j2"),
            Some(&root),
            &super::FileContext::discover(&root.join("templates/h.j2")),
        );
        let warm = match ask(uri).await {
            Some(tower_lsp::lsp_types::SemanticTokensResult::Tokens(t)) => t.data,
            other => panic!("{other:?}"),
        };
        assert!(
            warm.iter().any(|t| Some(t.token_type as usize) == kw),
            "the warm answer did not read the header: {warm:?}"
        );
    }

    /// Served for both surfaces now, and declined for anything else — a `.md` must not be
    /// painted with Jinja's legend just because it contains braces.
    #[tokio::test]
    async fn semantic_tokens_are_served_for_a_template_and_a_playbook_but_not_other_files() {
        let root = ansible_core::testing::project(
            "j2-tokens",
            "[defaults]\n",
            &[
                ("templates/t.j2", "{{ a | b }}\n"),
                ("play.yml", "- hosts: all\n  tasks:\n    - name: \"{{ app_name }}\"\n"),
                ("notes.md", "text with {{ braces }}\n"),
            ],
        );
        for (rel, want_some) in
            [("templates/t.j2", true), ("play.yml", true), ("notes.md", false)]
        {
            let got = tokens_for(&root, rel).await;
            assert_eq!(got.is_some(), want_some, "{rel}");
        }
    }

    /// The playbook half of T-217: `{{ }}` inside a YAML scalar, landing on the right
    /// columns.
    ///
    /// Asserted as decoded line/col/len rather than as the reader's return value — the
    /// protocol encodes each token as a delta from the previous one, and the delta
    /// arithmetic is where this has already gone wrong once (the encoder underflowed and
    /// panicked when the delimiters were emitted out of order).
    #[test]
    fn jinja_in_a_yaml_scalar_is_painted_at_the_right_columns() {
        let play = "- hosts: all\n  tasks:\n    - name: \"{{ app_name }}\"\n      when: flag is defined\n";
        let got = decoded_yaml(play);

        // `    - name: "{{ app_name }}"`
        //   col 12 is the quote, so `{{` is 13-14 and `app_name` is 16-23. The span the
        //   parser hands back excludes the quotes, which is what makes `span.start + offset`
        //   land inside them without any adjustment here.
        let line2: Vec<_> = got.iter().filter(|t| t.0 == 2).collect();
        assert!(
            line2.iter().any(|t| (t.1, t.2, t.3) == (13, 2, "delimiter")),
            "the opening delimiter, inside the quotes: {line2:?}"
        );
        assert!(
            line2.iter().any(|t| (t.1, t.2, t.3) == (16, 8, "variable")),
            "`app_name` as a variable at col 16: {line2:?}"
        );

        // `      when: flag is defined` — a bare expression, no delimiters to anchor to, so
        // reading it as a template would paint nothing at all.
        let line3: Vec<_> = got.iter().filter(|t| t.0 == 3).collect();
        assert!(
            line3.iter().any(|t| (t.1, t.2, t.3) == (12, 4, "variable")),
            "`flag` painted in a bare when: {line3:?}"
        );
        // `is` is a word operator and `defined` is a test — neither is a variable, and they
        // are not the same kind of thing either. Both were `keyword` when the three kinds
        // were lumped together.
        assert!(
            line3.iter().any(|t| t.3 == "wordOperator"),
            "`is` is a word operator: {line3:?}"
        );
        assert!(
            line3.iter().any(|t| (t.2, t.3) == (7, "function")),
            "`defined` is a test, which is function-shaped: {line3:?}"
        );
    }

    /// The guard that makes the offsets valid, and the reason it is a runtime identity check
    /// rather than a list of scalar styles.
    ///
    /// A block scalar's value is not its source slice — the `|` indicator and the block
    /// indent are stripped — so `span.start + offset` would paint columns that belong to
    /// other text. Measured before this was written: value `"literal {{ e }}\n"` against
    /// slice `"|\n    literal {{ e }}\n"`.
    #[test]
    fn a_block_scalar_is_left_alone_rather_than_painted_at_the_wrong_columns() {
        let got = decoded_yaml(
            "- hosts: all\n  vars:\n    a: \"{{ plain }}\"\n    b: |\n      {{ blocked }}\n",
        );
        assert!(
            got.iter().any(|t| t.0 == 2),
            "the plain scalar is still painted, so this test can fail: {got:?}"
        );
        assert!(
            !got.iter().any(|t| t.0 == 4),
            "nothing painted inside the block scalar: {got:?}"
        );
    }

    async fn tokens_for(
        root: &std::path::Path,
        rel: &str,
    ) -> Option<tower_lsp::lsp_types::SemanticTokensResult> {
        use tower_lsp::LanguageServer;
        let state = scan_state(root);
        let service = lsp_service(state);
        let uri = tower_lsp::lsp_types::Url::from_file_path(root.join(rel)).unwrap();
        let text = std::fs::read_to_string(root.join(rel)).unwrap();
        service
            .inner()
            .did_open(tower_lsp::lsp_types::DidOpenTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "yaml".into(),
                    version: 1,
                    text,
                },
            })
            .await;
        service
            .inner()
            .semantic_tokens_full(tower_lsp::lsp_types::SemanticTokensParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await
            .unwrap()
    }

    /// The wire carries **UTF-16** columns while a `Span` is UTF-8 bytes, and the two
    /// diverge on every non-ASCII character.
    ///
    /// Asserted by slicing the line *by UTF-16 units* at the column the wire reports: if the
    /// column is right that yields exactly the token's own text, and if it is off by the
    /// byte-vs-unit difference it yields neighbouring characters instead. An assertion on
    /// the number alone would need the arithmetic restated in the test, which is the same
    /// mistake twice rather than a check.
    ///
    /// `🎉` is the case that matters: 4 bytes of UTF-8 but **2** UTF-16 units, so a length
    /// or column computed either in bytes or in `chars()` comes out wrong, in two directions.
    #[test]
    fn columns_are_utf16_units_not_bytes_or_chars() {
        for (text, want) in [
            // é: 2 bytes, 1 unit.
            ("- hosts: all\n  tasks:\n    - name: \"h\u{e9}llo {{ x }}\"\n", "x"),
            // 🎉: 4 bytes, 2 units — a surrogate pair.
            ("- hosts: all\n  tasks:\n    - name: \"\u{1f389} {{ y }}\"\n", "y"),
            // Non-ASCII inside the expression, which used to panic outright.
            ("- hosts: all\n  tasks:\n    - name: \"{{ caf\u{e9}_port }}\"\n", "café_port"),
            // RTL, and a combining mark that is one char but two code points.
            ("- hosts: all\n  tasks:\n    - name: \"\u{5e9}\u{5dc}\u{5d5}\u{5dd} {{ z }}\"\n", "z"),
            ("- hosts: all\n  tasks:\n    - name: \"e\u{301} {{ w }}\"\n", "w"),
        ] {
            let line: Vec<u16> = text.lines().nth(2).unwrap().encode_utf16().collect();
            let got = decoded_yaml(text);
            let slice = |t: &(u32, u32, u32, &'static str)| {
                String::from_utf16_lossy(&line[t.1 as usize..(t.1 + t.2) as usize])
            };
            for t in got.iter().filter(|t| t.0 == 2) {
                let seg = slice(t);
                let expect = match t.3 {
                    "delimiter" => seg == "{{" || seg == "}}",
                    "variable" => seg == want,
                    _ => true,
                };
                assert!(expect, "token {t:?} slices to {seg:?} in {:?}", text.lines().nth(2));
            }
            assert!(
                got.iter().any(|t| t.3 == "variable" && slice(t) == want),
                "the variable must be found at all: {got:?}"
            );
        }
    }

    /// [`decoded`]'s sibling for the YAML surface. Same reason for existing: the deltas are
    /// unreadable, and two cancelling mistakes decode to a wrong absolute position that a
    /// raw-delta assertion would accept.
    fn decoded_yaml(text: &str) -> Vec<(u32, u32, u32, &'static str)> {
        let d = ansible_core::jinja::Delimiters::default();
        let (mut line, mut col) = (0u32, 0u32);
        super::Backend::yaml_semantic_tokens_of(text, &d)
            .into_iter()
            .map(|t| {
                line += t.delta_line;
                col = if t.delta_line == 0 { col + t.delta_start } else { t.delta_start };
                let name = super::SEMANTIC_TOKEN_LEGEND[t.token_type as usize].as_str();
                (line, col, t.length, name)
            })
            .collect()
    }

    fn lsp_service(state: std::sync::Arc<super::State>) -> tower_lsp::LspService<super::Backend> {
        let (service, _socket) =
            tower_lsp::LspService::new(move |client| super::Backend { client, state });
        service
    }

    fn scan_state(root: &std::path::Path) -> std::sync::Arc<super::State> {
        std::sync::Arc::new(super::State {
            docs: Default::default(),
            roots: std::sync::Mutex::new(vec![root.to_path_buf()]),
            flagged: Default::default(),
            render_sites: Default::default(),
            template_grammars: Default::default(),
            mutations: Default::default(),
            settings: Default::default(),
            inventory: Default::default(),
            startup_note: Default::default(),
            scanning: std::sync::atomic::AtomicBool::new(false),
            var_cache: no_cache(),
            scan_task: Default::default(),
            ansible_path: Default::default(),
            install: Default::default(),
        })
    }

    #[tokio::test]
    async fn the_workspace_scan_fills_the_servers_variable_cache() {
        let root = ansible_core::testing::project(
            "t201-scan-cache",
            "[defaults]\n",
            &[(
                "play.yml",
                "- hosts: all\n  vars:\n    scanned: 1\n  tasks:\n    - debug: {msg: \"{{ scanned }}\"}\n",
            )],
        );
        let state = scan_state(&root);

        // Control: an empty cache before the scan. Without it, "the cache has entries" would
        // also pass on a build where something else had already filled it.
        assert!(
            state.var_cache.lock().unwrap().entries.is_empty(),
            "nothing is cached before the scan runs"
        );

        let service = lsp_service(state.clone());
        super::Backend::scan_workspace(state.clone(), service.inner().client.clone()).await;

        let cached = state.var_cache.lock().unwrap();
        assert!(
            !cached.entries.is_empty(),
            "the scan must fill the server's cache — an empty one means it built its own"
        );
        assert!(
            cached.entries.keys().any(|(p, _)| p.ends_with("play.yml")),
            "and the entry is for the file it scanned: {:?}",
            cached.entries.keys().collect::<Vec<_>>()
        );
    }

    // ---- the lifecycle and notification handlers ----------------------------------------
    //
    // Every test above this point enters *below* the handler: it calls `set_inventory`,
    // `hover_at`, `analyze_text` directly. That proves the machinery works and says nothing
    // about whether the server ever calls it. Measured by mutation: seven wiring lines in
    // `initialize`, `did_change_configuration`, `did_close` and `publish_diagnostics` could be
    // deleted with the whole suite still green.
    //
    // These drive the handlers through the `Client` harness. Each one names the mutation it
    // exists to catch, because that is the only thing separating it from a decoration.

    /// A server on an empty fixture, for handler tests that need no project on disk.
    fn handler_server(name: &str) -> (tower_lsp::LspService<super::Backend>, std::path::PathBuf) {
        let root = ansible_core::testing::project(
            name,
            "[defaults]\n",
            &[("inv.ini", "[web]\nnode1\n"), ("play.yml", "- hosts: all\n")],
        );
        // Empty roots on purpose: recording them is `initialize`'s job, and a fixture that
        // pre-fills them would make that assertion vacuous.
        let state = std::sync::Arc::new(super::State {
            docs: Default::default(),
            roots: std::sync::Mutex::new(Vec::new()),
            flagged: Default::default(),
            render_sites: Default::default(),
            template_grammars: Default::default(),
            mutations: Default::default(),
            settings: Default::default(),
            inventory: Default::default(),
            startup_note: Default::default(),
            scanning: std::sync::atomic::AtomicBool::new(false),
            var_cache: no_cache(),
            scan_task: Default::default(),
            ansible_path: Default::default(),
            install: Default::default(),
        });
        (lsp_service(state), root)
    }

    fn init_params(root: &std::path::Path, opts: serde_json::Value) -> tower_lsp::lsp_types::InitializeParams {
        let mut p = tower_lsp::lsp_types::InitializeParams::default();
        p.workspace_folders = Some(vec![tower_lsp::lsp_types::WorkspaceFolder {
            uri: tower_lsp::lsp_types::Url::from_file_path(root).unwrap(),
            name: "fixture".into(),
        }]);
        p.initialization_options = Some(opts);
        p
    }

    /// Catches: `initialize` records no workspace roots.
    ///
    /// This is the one that matters most. Roots are what a relative `ansibleLsp.inventory`
    /// resolves against — with none recorded, nothing resolves, no inventory is read, and every
    /// inventory-derived variable silently disappears. That is T-201's failure one layer up.
    #[tokio::test]
    async fn initialize_records_the_workspace_folders_and_the_inventory_setting() {
        use tower_lsp::LanguageServer;
        let (service, root) = handler_server("t201-init-roots");
        let b = service.inner();

        // Control: nothing recorded before initialize runs.
        assert!(b.state.roots.lock().unwrap().is_empty(), "no roots before initialize");
        assert!(b.state.inventory.lock().unwrap().is_empty(), "no inventory before initialize");

        let opts = serde_json::json!({ "inventory": ["inv.ini"] });
        b.initialize(init_params(&root, opts)).await.expect("initialize succeeds");

        assert_eq!(
            *b.state.roots.lock().unwrap(),
            vec![root.clone()],
            "the folder the client sent must be recorded"
        );
        assert_eq!(
            *b.state.inventory.lock().unwrap(),
            vec![std::path::PathBuf::from("inv.ini")],
            "and initializationOptions must reach set_inventory"
        );
        // And the two together resolve, which is the thing either half being dropped breaks.
        assert_eq!(b.state.inventory_setting(), vec![root.join("inv.ini")]);
    }

    /// Catches: `did_change_configuration` ignores the new settings.
    ///
    /// Changing your inventory in settings and having the server keep answering from the old
    /// one is a silent wrong answer, not a missing feature.
    #[tokio::test]
    async fn did_change_configuration_applies_the_new_settings() {
        use tower_lsp::LanguageServer;
        let (service, root) = handler_server("t201-didchangeconfig");
        let b = service.inner();
        b.initialize(init_params(&root, serde_json::json!({ "inventory": ["inv.ini"] })))
            .await
            .expect("initialize succeeds");
        assert_eq!(b.state.inventory_setting(), vec![root.join("inv.ini")]);

        b.did_change_configuration(tower_lsp::lsp_types::DidChangeConfigurationParams {
            settings: serde_json::json!({
                "inventory": ["other.ini"],
                "inlayHints": { "enabled": false },
            }),
        })
        .await;

        assert_eq!(
            b.state.inventory_setting(),
            vec![root.join("other.ini")],
            "the new inventory must take effect"
        );
        assert!(
            !b.state.settings.lock().unwrap().hints,
            "and so must the rest of the settings blob"
        );
    }

    /// Catches: `did_close` leaves the buffer in the map.
    ///
    /// A closed file whose buffer survives means every later answer comes from text the editor
    /// is no longer showing — the T-199 bug with the sign flipped.
    #[tokio::test]
    async fn did_close_forgets_the_buffer() {
        use tower_lsp::LanguageServer;
        let (service, root) = handler_server("t201-didclose");
        let b = service.inner();
        let uri = tower_lsp::lsp_types::Url::from_file_path(root.join("play.yml")).unwrap();

        b.did_open(tower_lsp::lsp_types::DidOpenTextDocumentParams {
            text_document: tower_lsp::lsp_types::TextDocumentItem {
                uri: uri.clone(),
                language_id: "ansible".into(),
                version: 1,
                text: "- hosts: all\n".into(),
            },
        })
        .await;
        assert!(b.state.text_of(&uri).is_some(), "control: the buffer is open");

        b.did_close(tower_lsp::lsp_types::DidCloseTextDocumentParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        })
        .await;
        assert!(b.state.text_of(&uri).is_none(), "the buffer is gone after did_close");
    }

    /// Catches: `publish_diagnostics` stops tracking what it published.
    ///
    /// `flagged` is what the scan subtracts from to clear diagnostics that have become clean
    /// (`scan_workspace`, the `stale.difference(&still_flagged)` loop). If publishing stops
    /// recording, a warning that has been fixed never gets cleared from the editor.
    #[tokio::test]
    async fn publishing_diagnostics_records_which_files_are_flagged() {
        use tower_lsp::LanguageServer;
        let root = ansible_core::testing::project(
            "t201-flagged",
            "[defaults]\n",
            &[("play.yml", "- hosts: all\n  tasks:\n    - debug: {msg: x}\n      loop_control:\n        label: y\n")],
        );
        let state = scan_state(&root);
        let service = lsp_service(state);
        let b = service.inner();
        let uri = tower_lsp::lsp_types::Url::from_file_path(root.join("play.yml")).unwrap();

        assert!(b.state.flagged.lock().unwrap().is_empty(), "control: nothing flagged yet");

        b.did_open(tower_lsp::lsp_types::DidOpenTextDocumentParams {
            text_document: tower_lsp::lsp_types::TextDocumentItem {
                uri: uri.clone(),
                language_id: "ansible".into(),
                version: 1,
                text: std::fs::read_to_string(root.join("play.yml")).unwrap(),
            },
        })
        .await;

        // `loop_control:` with no `loop:` is dead config (T-155), so this file has a
        // diagnostic and must be recorded as flagged.
        assert!(
            b.state.flagged.lock().unwrap().contains(&uri),
            "a file that published diagnostics must be tracked as flagged"
        );
    }

    /// The status bar's answer, asserted as a value rather than as a notification.
    ///
    /// T-062's whole argument is that a tool which picks an inventory silently reproduces the
    /// ambiguity it exists to solve — so "which one won, and why" is a claim the tool makes on
    /// screen, and an unpinned claim is how a wrong one survives. Before the
    /// `publish_inventory`/`inventory_status` split there was no way to assert it without
    /// draining the client socket, and nothing did.
    #[tokio::test]
    async fn the_status_bar_names_the_setting_over_the_config_and_says_which_config_it_read() {
        use tower_lsp::LanguageServer;
        let root = ansible_core::testing::project(
            "t201-status",
            "[defaults]\ninventory = from_cfg.ini\n",
            &[
                ("from_cfg.ini", "[web]\nnode1\n"),
                ("from_setting.ini", "[web]\nnode2\n"),
                ("play.yml", "- hosts: all\n"),
            ],
        );
        let state = std::sync::Arc::new(super::State {
            docs: Default::default(),
            roots: std::sync::Mutex::new(Vec::new()),
            flagged: Default::default(),
            render_sites: Default::default(),
            template_grammars: Default::default(),
            mutations: Default::default(),
            settings: Default::default(),
            inventory: Default::default(),
            startup_note: Default::default(),
            scanning: std::sync::atomic::AtomicBool::new(false),
            var_cache: no_cache(),
            scan_task: Default::default(),
            ansible_path: Default::default(),
            install: Default::default(),
        });
        let service = lsp_service(state.clone());
        let b = service.inner();

        // Control: with nothing configured the tool must credit `ansible.cfg`, not itself.
        b.initialize(init_params(&root, serde_json::json!({}))).await.expect("initialize");
        let auto = super::Backend::inventory_status(&state);
        assert_eq!(auto["source"], "ansible.cfg", "unconfigured, the cfg wins: {auto}");
        assert_eq!(
            auto["resolved"],
            serde_json::json!(["from_cfg.ini"]),
            "and it resolves to the cfg's file: {auto}"
        );

        // Now the user states their `-i`. It is the top rung, so it must beat the cfg — and
        // the control above is what makes this assertion mean something rather than restating
        // the fixture.
        b.did_change_configuration(tower_lsp::lsp_types::DidChangeConfigurationParams {
            settings: serde_json::json!({ "inventory": ["from_setting.ini"] }),
        })
        .await;

        let chosen = super::Backend::inventory_status(&state);
        assert_eq!(chosen["source"], "ansibleLsp.inventory", "the setting wins: {chosen}");
        assert_eq!(
            chosen["resolved"],
            serde_json::json!(["from_setting.ini"]),
            "and it is the file actually read: {chosen}"
        );
        // The cfg is still named, because "what you would get without the setting" is the
        // other half of the ambiguity the status bar exists to remove.
        assert_eq!(
            chosen["autoResolved"],
            serde_json::json!(["from_cfg.ini"]),
            "what a plain ansible-playbook would read is still reported: {chosen}"
        );
        assert_eq!(
            chosen["configFile"], "ansible.cfg",
            "and which cfg was read is named, not just that one was: {chosen}"
        );
    }

    // ---- reading what the server actually sent ------------------------------------------
    //
    // `publish_inventory` and `publish_diagnostics` return nothing: the outbound notification
    // *is* the result. Every test above asserts on the value that goes into one, which leaves
    // the handing-over unpinned — delete the `publish_inventory` call from
    // `did_change_configuration` and the status bar silently goes stale, suite green.
    //
    // Draining deterministically, with no sleep and no timeout: the collector runs on its own
    // task and its stream ends when the last `Client` is dropped, which happens when the
    // service is. So `drop(service)` is the signal, not elapsed time — a timeout-based drain
    // would be a flaky test inside a ticket about flaky tests.

    /// Route a real `initialize` request through the service.
    ///
    /// Not the same as calling `Backend::initialize` directly: the *service layer* is what
    /// records the server as initialized (`service/layers.rs:73`), and until it does,
    /// `Client::send_notification` silently drops everything (`service/client.rs:441`). A test
    /// that skips this sees an empty message list and reads it as "the server sent nothing".
    async fn initialize_through_service(
        service: &mut tower_lsp::LspService<super::Backend>,
        root: &std::path::Path,
    ) {
        use tower::{Service, ServiceExt};
        let req = tower_lsp::jsonrpc::Request::build("initialize")
            .params(serde_json::json!({
                "capabilities": {},
                "workspaceFolders": [{
                    "uri": tower_lsp::lsp_types::Url::from_file_path(root).unwrap(),
                    "name": "fixture",
                }],
            }))
            .id(1)
            .finish();
        service
            .ready()
            .await
            .expect("service is ready")
            .call(req)
            .await
            .expect("initialize is routed");
    }

    /// A project whose `ansible.cfg` names an inventory, plus a state rooted at it.
    fn sent_fixture(name: &str) -> (std::sync::Arc<super::State>, std::path::PathBuf) {
        let root = ansible_core::testing::project(
            name,
            "[defaults]\ninventory = inv.ini\n",
            &[("inv.ini", "[web]\nnode1\n"), ("play.yml", "- hosts: all\n")],
        );
        let state = std::sync::Arc::new(super::State {
            docs: Default::default(),
            roots: std::sync::Mutex::new(vec![root.clone()]),
            flagged: Default::default(),
            render_sites: Default::default(),
            template_grammars: Default::default(),
            mutations: Default::default(),
            settings: Default::default(),
            inventory: Default::default(),
            startup_note: Default::default(),
            scanning: std::sync::atomic::AtomicBool::new(false),
            var_cache: no_cache(),
            scan_task: Default::default(),
            ansible_path: Default::default(),
            install: Default::default(),
        });
        (state, root)
    }

    /// Catches: `did_change_configuration` does not re-publish the inventory.
    ///
    /// T-062's argument is that a silently-chosen inventory reproduces the ambiguity the tool
    /// exists to remove. A status bar still showing the *previous* inventory after the user
    /// changes the setting is that same lie, one step later.
    #[tokio::test]
    async fn changing_the_settings_re_publishes_the_inventory_to_the_client() {
        use futures::StreamExt;
        use tower_lsp::LanguageServer;

        let (state, root) = sent_fixture("t201-republish");
        let (mut service, socket) =
            tower_lsp::LspService::new(move |client| super::Backend { client, state });
        let drain =
            tokio::spawn(async move { socket.map(|r| r.method().to_string()).collect::<Vec<_>>().await });
        initialize_through_service(&mut service, &root).await;

        service
            .inner()
            .did_change_configuration(tower_lsp::lsp_types::DidChangeConfigurationParams {
                settings: serde_json::json!({ "inventory": ["inv.ini"] }),
            })
            .await;

        drop(service); // closes the channel, so the collect below terminates
        let sent = drain.await.expect("the drain task does not panic");

        assert!(
            sent.iter().any(|m| m == "ansible/inventory"),
            "the settings change must reach the status bar: {sent:?}"
        );
    }

    /// Catches: `publish_diagnostics` never sends.
    ///
    /// The counterpart to the test above — `did_open` computes diagnostics and hands them to
    /// the client, and nothing else in the suite watches the handing-over.
    #[tokio::test]
    async fn opening_a_file_publishes_its_diagnostics_to_the_client() {
        use futures::StreamExt;
        use tower_lsp::LanguageServer;

        let (state, root) = sent_fixture("t201-publishdiag");
        let (mut service, socket) =
            tower_lsp::LspService::new(move |client| super::Backend { client, state });
        let drain =
            tokio::spawn(async move { socket.map(|r| r.method().to_string()).collect::<Vec<_>>().await });
        initialize_through_service(&mut service, &root).await;

        service
            .inner()
            .did_open(tower_lsp::lsp_types::DidOpenTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentItem {
                    uri: tower_lsp::lsp_types::Url::from_file_path(root.join("play.yml")).unwrap(),
                    language_id: "ansible".into(),
                    version: 1,
                    text: "- hosts: all\n".into(),
                },
            })
            .await;

        drop(service);
        let sent = drain.await.expect("the drain task does not panic");

        assert!(
            sent.iter().any(|m| m == "textDocument/publishDiagnostics"),
            "opening a file must publish its diagnostics: {sent:?}"
        );
    }

    /// Catches: `initialized` never starts the scan.
    ///
    /// `initialized` spawns and returns, so there is nothing to observe at the moment it
    /// finishes — which is why this line survived every earlier mutation. The handle is the
    /// fix: awaiting it returns exactly when the scan is done, with no sleep and no polling
    /// for an effect that may not have happened yet.
    #[tokio::test]
    async fn initialized_starts_the_workspace_scan() {
        use tower_lsp::LanguageServer;

        let root = ansible_core::testing::project(
            "t201-initialized-scan",
            "[defaults]\n",
            &[("play.yml", "- hosts: all\n  vars:\n    scanned: 1\n")],
        );
        let state = scan_state(&root);
        let service = lsp_service(state.clone());
        let b = service.inner();

        // Control: the scan has not run, so nothing is cached and no task exists.
        assert!(state.var_cache.lock().unwrap().entries.is_empty(), "nothing scanned yet");
        assert!(state.scan_task.lock().unwrap().is_none(), "and no scan task yet");

        b.initialized(tower_lsp::lsp_types::InitializedParams {}).await;

        let task = state
            .scan_task
            .lock()
            .unwrap()
            .take()
            .expect("initialized must start the workspace scan");
        task.await.expect("the scan task runs to completion without panicking");

        assert!(
            !state.var_cache.lock().unwrap().entries.is_empty(),
            "and the scan it started must have indexed the workspace"
        );
    }

    /// Catches: `initialize` ignores `ansiblePath`.
    ///
    /// The last of the ten wiring mutations to be pinned, and it needed T-201 box (5) first:
    /// the setting used to be written into `install::OVERRIDE`, a `pub(crate)` `OnceLock` with
    /// no getter, so nothing could read back whether it had arrived. Now it is recorded on
    /// `State` and handed to `AnsibleInstall::init` by `startup`.
    ///
    /// Asserted at `State`, not at the install: `init` is `OnceLock`-backed and process-wide,
    /// so a test that actually ran detection would decide the answer for every other test in
    /// the binary — which is the very hazard this ticket exists to remove. What is checked is
    /// the half that was missing, that the setting reaches the value `startup` reads.
    #[tokio::test]
    async fn initialize_records_the_ansible_path_setting() {
        use tower_lsp::LanguageServer;
        let (service, root) = handler_server("t201-ansiblepath");
        let b = service.inner();

        assert!(b.state.ansible_path().is_none(), "control: nothing recorded before initialize");

        let pkg = root.join("venv/lib/site-packages/ansible");
        b.initialize(init_params(
            &root,
            serde_json::json!({ "ansiblePath": pkg.to_string_lossy() }),
        ))
        .await
        .expect("initialize succeeds");

        assert_eq!(
            b.state.ansible_path(),
            Some(pkg),
            "the ansiblePath setting must reach the value startup hands to AnsibleInstall::init"
        );
    }

    /// Blank and whitespace-only spellings mean "not set", not "index the workspace root".
    /// The same filtering `ansibleLsp.inventory` does, for the same reason: an empty path
    /// resolves to a directory that is not an Ansible install, and detection would silently
    /// fall through to the PATH walk-up with no way to tell the two apart.
    #[tokio::test]
    async fn a_blank_ansible_path_is_no_setting_at_all() {
        use tower_lsp::LanguageServer;
        for blank in [serde_json::json!(""), serde_json::json!("   "), serde_json::json!(null)] {
            let (service, root) = handler_server("t201-ansiblepath-blank");
            let b = service.inner();
            b.initialize(init_params(&root, serde_json::json!({ "ansiblePath": blank })))
                .await
                .expect("initialize succeeds");
            assert!(
                b.state.ansible_path().is_none(),
                "{blank} should leave the setting unset"
            );
        }
    }

    /// A multi-root window must answer each folder from *its own* inventory (T-202).
    ///
    /// Ignored because it asserts the answer we want, not the one we give. `inventory_setting`
    /// resolves a relative `ansibleLsp.inventory` against `roots.first()`, so today a file in
    /// folder B is answered from folder A's `inv.ini` — a wrong value **and** a source link
    /// into the wrong project. Measured, and unchanged by T-201, which moved the value off a
    /// process global without touching the rule.
    ///
    /// Written now rather than with the fix so T-202 has the shape to work against and can be
    /// judged by un-ignoring it. It covers only the case T-202 has already decided: a file
    /// under exactly one root. The nested case — folder B *inside* folder A, which VS Code
    /// allows — is deliberately absent, because the rule for it is still open and a test
    /// asserting a guess would be worse than no test.
    ///
    /// Un-ignore this when T-202 lands. If it needs editing to pass, the rule changed and the
    /// ticket should say why.
    #[tokio::test]
    #[ignore = "asserts the multi-root answer we do not give yet — T-202"]
    async fn each_workspace_folder_answers_from_its_own_inventory() {
        let mk = |name: &str, val: &str| {
            ansible_core::testing::project(
                name,
                "[defaults]\n",
                &[
                    ("inv.ini", &format!("[web]\nnode1 control={val}\n")),
                    ("vars/11.yml", "x: 1\n"),
                    ("vars/22.yml", "x: 1\n"),
                    ("play.yml", T201_PLAY),
                ],
            )
        };
        let a = mk("t202-folder-a", "11");
        let b = mk("t202-folder-b", "22");

        // One window holding both folders, in the order the client sent them, with a single
        // window-scoped relative setting — the shape `ansibleLsp.inventory` actually has.
        let state = std::sync::Arc::new(super::State {
            docs: Default::default(),
            roots: std::sync::Mutex::new(vec![a.clone(), b.clone()]),
            flagged: Default::default(),
            render_sites: Default::default(),
            template_grammars: Default::default(),
            mutations: Default::default(),
            settings: Default::default(),
            inventory: Default::default(),
            startup_note: Default::default(),
            scanning: std::sync::atomic::AtomicBool::new(false),
            var_cache: no_cache(),
            scan_task: Default::default(),
            ansible_path: Default::default(),
            install: Default::default(),
        });
        state.set_inventory(&serde_json::json!({ "inventory": ["inv.ini"] }));

        // Folder A is `roots.first()`, so this one already passes today. It is the control:
        // without it, "B is wrong" could equally mean no inventory was read at all.
        let from_a = t201_hover(&a, &state.inventory_setting(), &no_cache());
        assert!(
            from_a.contains("`control` = `11`") && from_a.contains("t202-folder-a"),
            "folder A answers from its own inventory:\n{from_a}"
        );

        // The bug: folder B is answered from folder A.
        let from_b = t201_hover(&b, &state.inventory_setting(), &no_cache());
        assert!(
            from_b.contains("`control` = `22`"),
            "folder B must answer from its own inventory, not folder A's:\n{from_b}"
        );
        assert!(
            from_b.contains("t202-folder-b"),
            "and the source link must point inside folder B:\n{from_b}"
        );
    }
}
