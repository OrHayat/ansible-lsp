//! Thin LSP shim over `ansible-core`. All logic lives in the core crate.

mod md;

use md::{Md, Prose};

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use ansible_core::cache::ScanCache;
use ansible_core::config::DuplicateDictKey;
use ansible_core::expressions;
use ansible_core::fs::{Counting, StdFs};
use ansible_core::install::{AnsibleInstall, Version};
use ansible_core::attributes;
use ansible_core::condition;
use ansible_core::mutation;
use ansible_core::parse::{Document, Loader, Node, Span};
use ansible_core::placement;
use ansible_core::references::{self, Reference, ReferenceKind};
use ansible_core::resolve::{self, rule_id, Resolution, SkipReason, Status};
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

// ---------------------------------------------------------- variable cache (T-055)
// `vars::definitions` walks the filesystem (reads and parses every included file, role
// vars, group_vars…) and is called from several per-keystroke paths. Cache each file's
// result together with the set of files it read, and keep a reverse map file -> dependents,
// so an edit invalidates exactly the entries that read the changed file — and nothing else.
#[derive(Default)]
struct VarCache {
    entries: HashMap<PathBuf, Arc<Vec<vars::Located>>>,
    deps: HashMap<PathBuf, HashSet<PathBuf>>,
    reverse: HashMap<PathBuf, HashSet<PathBuf>>,
    /// Bumped by every invalidation. The scan (detached since T-075) computes entries from
    /// disk while edits arrive; an entry whose compute straddled an invalidation must not
    /// be inserted, or a result read from pre-edit content outlives the edit.
    epoch: u64,
}

fn var_cache() -> &'static Mutex<VarCache> {
    static C: OnceLock<Mutex<VarCache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(VarCache::default()))
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Cached `vars::definitions`. On a miss, compute it and record its dependency files in the
/// reverse map so later invalidation is precise.
fn cached_definitions(path: &Path, nodes: &[Node]) -> Arc<Vec<vars::Located>> {
    cached_definitions_in(path, nodes, &ScanCache::default())
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
) -> Arc<Vec<vars::Located>> {
    let key = canon(path);
    let epoch = match var_cache().lock() {
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
    if let Ok(mut cache) = var_cache().lock() {
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
fn invalidate_var_cache(file: &Path) {
    let f = canon(file);
    let Ok(mut cache) = var_cache().lock() else {
        return;
    };
    cache.epoch = cache.epoch.wrapping_add(1);
    let mut keys: HashSet<PathBuf> = cache.reverse.get(&f).cloned().unwrap_or_default();
    keys.insert(f);
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
    /// What arrived in `initializationOptions`, logged once the client can receive it.
    startup_note: Mutex<String>,
    /// True while a workspace scan runs — a second trigger during one would double-publish.
    scanning: AtomicBool,
}

struct Backend {
    client: Client,
    state: Arc<State>,
}

struct Analysis {
    doc: Document,
    nodes: Vec<Node>,
    ctx: FileContext,
    refs: Vec<(Reference, Resolution)>,
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

    /// Parse `uri` and resolve every reference in it. `None` when the file isn't open,
    /// isn't a real path, or doesn't parse.
    fn analyze(&self, uri: &Url) -> Option<Analysis> {
        let text = self.text_of(uri)?;
        Backend::analyze_text(text, &uri.to_file_path().ok()?)
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

    /// One ERROR at the parse-error position when an open file isn't valid YAML. The parser
    /// matches Ansible (libyaml), so this is invalid for Ansible too — a play that loads the
    /// file will fail. Empty when the file is fine, isn't open, or is `# noqa`-suppressed.
    fn unparseable_diagnostic(&self, uri: &Url) -> Vec<Diagnostic> {
        let Some(text) = self.text_of(uri) else {
            return Vec::new();
        };
        let doc = Document::new(text);
        let Some(span) = doc.parse_error() else {
            return Vec::new();
        };
        if doc.is_suppressed(span.start, "unparseable") {
            return Vec::new();
        }
        let (sl, sc) = doc.byte_to_lsp(span.start);
        let (el, ec) = doc.byte_to_lsp(span.end);
        vec![Diagnostic {
            range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("ansible-lsp".into()),
            code: Some(NumberOrString::String("unparseable".into())),
            message: "Invalid YAML — Ansible's parser rejects this too, so a play that loads \
                      this file will fail. References here aren't analysed."
                .into(),
            ..Default::default()
        }]
    }

    /// A `when:` on `import_playbook` whose variable the imported playbook itself sets.
    ///
    /// The condition is copied onto every imported task and re-evaluated per task, so a
    /// `set_fact` inside flips it mid-run: everything before runs, everything after
    /// silently skips. `set_fact` is host-scoped, so a cluster can split. This is the
    /// only `when:` rule that needs to read other files.
    fn mutated_condition_diagnostics(&self, a: &Analysis) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for (r, res) in &a.refs {
            if r.kind != ReferenceKind::ImportPlaybook || r.conditions.is_empty() {
                continue;
            }
            let Some(span) = r.condition_span else { continue };
            if a.doc.is_suppressed(span.start, "when-import-var-mutated") {
                continue;
            }
            let used: Vec<String> = r
                .conditions
                .iter()
                .flat_map(|c| condition::variables(c))
                .collect();
            if used.is_empty() {
                continue;
            }
            for target in &res.targets {
                let mutated = self.mutated_vars(target);
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
    fn analyze_text(text: String, path: &Path) -> Option<Analysis> {
        Self::analyze_text_measured(text, path, &mut ScanTimings::default(), &ScanCache::default())
    }

    /// The body of `analyze_text`, wrapping each phase with a timer that accumulates into
    /// `t`. The un-instrumented `analyze_text` passes a throwaway accumulator and its own
    /// one-shot cache, so the logic lives in exactly one place.
    fn analyze_text_measured(
        text: String,
        path: &Path,
        t: &mut ScanTimings,
        scan: &ScanCache,
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
        let defs = cached_definitions_in(path, &nodes, scan);
        let literals = vars::known_literals_in(&defs, path, &doc.text, scan);
        t.var_index += s.elapsed();

        let s = Instant::now();
        let mut extracted = references::extract(&nodes);
        if path.ends_with("meta/main.yml") && ctx.role_dir.is_some() {
            extracted.extend(references::meta_dependencies(&nodes));
        }
        let refs = extracted
            .into_iter()
            .map(|r| {
                let res = resolve::resolve_with_in(&r, &ctx, &literals, scan);
                (r, res)
            })
            .collect();
        t.resolve += s.elapsed();

        Some(Analysis { doc, nodes, ctx, refs })
    }

    /// Warn only on literal paths that resolved to nothing. Templated values and
    /// unsupported kinds stay silent — a warning you can't trust is worse than none.
    async fn publish_diagnostics(&self, uri: &Url) {
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
            diagnostics.extend(Self::variable_coverage_diagnostics(&a, &path, &a.nodes));
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

    fn diagnostics_of(a: &Analysis) -> Vec<Diagnostic> {
        Self::diagnostics_with(a, AnsibleInstall::detected().and_then(|i| i.version))
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
            .filter(|(r, _)| !a.doc.is_suppressed(r.span.start, rule_id(r)))
            .map(|(r, res)| Diagnostic {
                range: range_of(r.span),
                severity: Some(DiagnosticSeverity::WARNING),
                source: Some("ansible-lsp".into()),
                code: Some(NumberOrString::String(rule_id(r).into())),
                message: message_for(r, res, &a.ctx),
                ..Default::default()
            });

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
        let invalid: Vec<Diagnostic> = attributes::problems(
            &ansible_core::ast::build(&a.nodes),
            a.ctx.config.invalid_task_attribute_failed,
        )
        .into_iter()
        .filter(|p| !a.doc.is_suppressed(p.span.start, attributes::RULE_ID))
        .map(|p| Diagnostic {
            range: range_of(p.span),
            severity: Some(match p.tier {
                attributes::Tier::Error => DiagnosticSeverity::ERROR,
                attributes::Tier::Warning => DiagnosticSeverity::WARNING,
            }),
            source: Some("ansible-lsp".into()),
            code: Some(NumberOrString::String(attributes::RULE_ID.into())),
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
                }),
                source: Some("ansible-lsp".into()),
                code: Some(NumberOrString::String(p.rule.into())),
                message: p.message,
                ..Default::default()
            })
            .collect();

        missing.chain(broken).chain(invalid).chain(misplaced).collect()
    }

    /// Condition-aware definedness: a variable *used* under a `when:` that its *definitions*
    /// don't all cover. If the use can run in a case where no in-effect definition applies —
    /// e.g. used for `web01 or web02` but only registered on `web01` — warn, naming the
    /// uncovered case. Only fires when there IS a definition (a coverage gap, not "never
    /// defined") and only within the use's own condition vocabulary, so it can't false-warn
    /// on conditions it can't relate. Suppressible with `# noqa: var-uncovered-when`.
    fn variable_coverage_diagnostics(a: &Analysis, path: &Path, nodes: &[Node]) -> Vec<Diagnostic> {
        let defs = cached_definitions(path, nodes);
        let mut out = Vec::new();
        for u in vars::uses(nodes) {
            if u.guard.is_empty() {
                continue;
            }
            // Definitions of this name that are in effect at the use.
            let def_guards: Vec<Vec<String>> = defs
                .iter()
                .filter(|d| d.name == u.name && d.in_effect_at(path, u.span.start))
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
                message: format!(
                    "`{}` is never defined in any file reachable from this playbook — it may \
                     still come from inventory, facts, or extra-vars (-e).",
                    u.name
                ),
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
        let install = tokio::task::spawn_blocking(|| {
            ansible_core::install::AnsibleInstall::detect().clone()
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
        Self::scan_workspace(state, client).await;
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
        let scan_cache = Arc::new(ScanCache::new(disk.clone()));

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
                set.spawn_blocking(move || {
                    let mut ft = ScanTimings::default();
                    let text = std::fs::read_to_string(&path).ok()?;
                    let a = Self::analyze_text_measured(text, &path, &mut ft, &sc)?;
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
            let all = cached_definitions(&path, nodes);
            for u in vars::uses(nodes) {
                // In-effect count depends on the use position (a later set_fact hasn't run),
                // so it's computed per use rather than once per name.
                let n = all
                    .iter()
                    .filter(|d| d.name == u.name && d.in_effect_at(&path, u.span.start))
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

    fn reference_at(doc: &Document, nodes: &[Node], pos: Position) -> Option<Reference> {
        let byte = doc.lsp_to_byte(pos.line, pos.character);
        references::extract(nodes)
            .into_iter()
            .find(|r| r.span.start <= byte && byte <= r.span.end)
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
    ) -> Option<Vec<Location>> {
        let byte = doc.lsp_to_byte(pos.line, pos.character);
        let use_ = vars::uses(nodes)
            .into_iter()
            .find(|u| byte >= u.span.start && byte < u.span.end)?;
        let path = uri.to_file_path().ok()?;
        let defs: Vec<vars::Located> = cached_definitions(&path, nodes)
            .iter()
            .filter(|d| d.name == use_.name && d.in_effect_at(&path, use_.span.start))
            .cloned()
            .collect();
        // Jump to the definition that actually applies here — highest precedence, latest on a
        // tie — rather than a picker of every assignment. (hover lists them all, ranked.)
        let d = vars::effective(&defs)?;
        // The definition's span is in *its own* file: current file from the in-memory
        // (possibly unsaved) buffer, others read from disk.
        let target = if d.file == path {
            Document::new(doc.text.clone())
        } else {
            Document::new(std::fs::read_to_string(&d.file).ok()?)
        };
        let (sl, sc) = target.byte_to_lsp(d.span.start);
        let (el, ec) = target.byte_to_lsp(d.span.end);
        let u = Url::from_file_path(&d.file).ok()?;
        Some(vec![Location {
            uri: u,
            range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
        }])
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
    ) -> Option<(String, Range)> {
        let idents = template_idents(&r.value);
        if idents.is_empty() {
            return None;
        }
        let defs = cached_definitions(path, nodes);
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
                    Document::new(std::fs::read_to_string(&d.file).unwrap_or_default())
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
        let md = injected_var_hover(&use_.name, install?)?;
        let (sl, sc) = doc.byte_to_lsp(use_.span.start);
        let (el, ec) = doc.byte_to_lsp(use_.span.end);
        Some((md, Range::new(Position::new(sl, sc), Position::new(el, ec))))
    }

    fn variable_hover_at(
        doc: &Document,
        nodes: &[Node],
        byte: usize,
        path: &Path,
    ) -> Option<(String, Range)> {
        // One scan for both kinds of name. The rule-facing `vars::uses` drops the injected
        // ones, so asking it first and falling back to a second, complementary scan walked
        // the whole tree twice for every token that is not an ordinary variable.
        let use_ = vars::any_uses(nodes)
            .into_iter()
            .find(|u| byte >= u.span.start && byte < u.span.end)?;
        if condition::is_injected(&use_.name) {
            return Self::injected_var_hover_at(doc, &use_, AnsibleInstall::detected());
        }
        let mut defs: Vec<vars::Located> = cached_definitions(path, nodes)
            .iter()
            .filter(|d| d.name == use_.name && d.in_effect_at(path, use_.span.start))
            .cloned()
            .collect();
        if defs.is_empty() {
            return None;
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
                        Document::new(std::fs::read_to_string(vf).unwrap_or_default())
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
                    Document::new(std::fs::read_to_string(&d.file).unwrap_or_default())
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
        IncludeVars => "include_vars",
    }
}

/// The literal value of a definition, when its span points at a value (play/block/task vars,
/// vars_files, role defaults/vars). `set_fact`/`register` spans point at the name, so those
/// carry no value here. Whitespace-collapsed and length-capped for a one-line hover.
fn def_value(d: &vars::Located, text: &str) -> Option<String> {
    use vars::VarSource::*;
    match d.source {
        PlayVars | BlockVars | TaskVars | VarsFiles | RoleDefaults | RoleVars
        | GroupVarsAll | GroupVars | HostVars | IncludeVars => {
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
            let builtin = ansible_core::install::AnsibleInstall::detect()
                .package_dir
                .as_ref()
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
fn injected_var_hover(name: &str, install: &AnsibleInstall) -> Option<String> {
    let source = || match install.package_dir.as_ref() {
        Some(p) => md::text(&format!("from the detected install at {}", p.display())),
        None => md::text("from the detected install"),
    };
    match name {
        "ansible_playbook_python" => {
            let py = install.python.as_ref()?;
            Some(
                Md::new()
                    .line(md::text("`ansible_playbook_python` — the interpreter Ansible runs on"))
                    .line(md::code(&py.display().to_string()))
                    .gap()
                    .line(source().italic())
                    .line(
                        md::text(
                            "The running play's own interpreter may differ — this is the one \
                             behind the `ansible` this editor found.",
                        )
                        .italic(),
                    )
                    .render(),
            )
        }
        // A dict at runtime (`full`, `major`, `minor`, `revision`, `string`), so the hover
        // reports the release rather than implying the bare name is a string.
        "ansible_version" => {
            let v = install.version.as_ref()?;
            Some(
                Md::new()
                    .line(md::text("`ansible_version` — ansible-core, as a dict"))
                    .line(md::code(&format!("{v}")))
                    .gap()
                    .line(source().italic())
                    .render(),
            )
        }
        _ => None,
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
) -> Option<(String, Range)> {
    let ctx = FileContext::discover(path);
    let range = |s: Span| {
        let (sl, sc) = doc.byte_to_lsp(s.start);
        let (el, ec) = doc.byte_to_lsp(s.end);
        Range::new(Position::new(sl, sc), Position::new(el, ec))
    };

    // The reference under the cursor, extracted but not yet resolved. Resolving every
    // reference in the file to answer a hover on one is the cost this path avoids:
    // only the hovered reference is resolved, and only if a branch below needs it.
    let mut refs = references::extract(nodes);
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
        let defs = cached_definitions(path, nodes);
        let literals = vars::known_literals(&defs, path, &doc.text);
        let res = resolve::resolve_with(r, &ctx, &literals);
        if r.templated {
            if let Some(hit) = Backend::path_substitution_hover(doc, nodes, r, &res, path) {
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
    Backend::variable_hover_at(doc, nodes, byte, path)
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
                // Which Ansible to index, when several exist or none is on PATH. Read here,
                // before the first `detect()` in `initialized`, so the setting wins.
                if let Some(path) = opts
                    .get("ansiblePath")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    ansible_core::install::set_package_dir_override(PathBuf::from(path));
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
        tokio::spawn(Self::startup(self.state.clone(), self.client.clone()));
    }

    async fn did_change_configuration(&self, p: DidChangeConfigurationParams) {
        if let Ok(mut s) = self.state.settings.lock() {
            *s = Settings::from_json(&p.settings);
        }
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
        Ok(
            hover_at(&doc, &nodes, &path, byte, settings).map(|(value, range)| Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }),
                range: Some(range),
            }),
        )
    }

    async fn did_open(&self, p: DidOpenTextDocumentParams) {
        let uri = p.text_document.uri;
        if let Ok(path) = uri.to_file_path() {
            invalidate_var_cache(&path);
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
            invalidate_var_cache(&path);
        }
        if let Ok(mut d) = self.state.docs.lock() {
            d.insert(uri.clone(), change.text);
        }
        self.publish_diagnostics(&uri).await;
    }

    async fn did_close(&self, p: DidCloseTextDocumentParams) {
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

        let doc = Document::new(text);
        // Unparseable is expected, not an error: strict YAML 1.2 rejects files the
        // PyYAML Ansible uses accepts. Return nothing rather than guessing.
        let Some(nodes) = doc.parse() else {
            return Ok(None);
        };
        let Some(reference) = Self::reference_at(&doc, &nodes, pos) else {
            // Not on a file/role/module reference — maybe on a variable use. Jump to where
            // it's defined in this file (cross-file sources are a later step).
            if let Some(locs) = Self::variable_defs_at(&doc, &nodes, pos, &uri) {
                return Ok(Some(GotoDefinitionResponse::Array(locs)));
            }
            return Ok(None);
        };

        let ctx = FileContext::discover(&path);
        let res = resolve::resolve(&reference, &ctx);
        if res.status != Status::Resolved {
            return Ok(None);
        }

        let locations: Vec<Location> = res.targets.iter().filter_map(|t| location_at(t)).collect();
        if locations.is_empty() {
            return Ok(None);
        }
        Ok(Some(GotoDefinitionResponse::Array(locations)))
    }

    /// Every resolvable reference. The client also paints these, so what's clickable is
    /// visible without hovering.
    async fn document_link(&self, p: DocumentLinkParams) -> Result<Option<Vec<DocumentLink>>> {
        let Some(a) = self.state.analyze(&p.text_document.uri) else {
            return Ok(None);
        };
        let links = a
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
        Ok(Some(links))
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
            mutations: Mutex::new(HashMap::new()),
            settings: Mutex::new(Settings::default()),
            startup_note: Mutex::new(String::new()),
            scanning: AtomicBool::new(false),
        }),
    })
    .custom_method("ansible/references", Backend::resolved_references)
    .finish();
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use super::Settings;

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
            if !ansible_core::condition::is_injected(&use_.name) {
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
        // No install detected yet, or one we learned nothing about: silence, not a guess.
        assert!(hover("ansible_playbook_python", None).is_none());
        assert!(hover("ansible_version", Some(&AnsibleInstall::default())).is_none());
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
                .filter(|u| ansible_core::condition::is_injected(&u.name))
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
        let hover = |n: &str| super::injected_var_hover(n, &install);

        let py = hover("ansible_playbook_python").expect("interpreter is known");
        assert!(py.contains("/venv/bin/python"), "{py}");
        assert!(py.contains("/venv/lib/python3.13/site-packages/ansible"), "names its source");
        assert!(py.contains("may differ"), "concedes the play may run elsewhere");

        let v = hover("ansible_version").expect("version is known");
        assert!(v.contains("2.21.2"), "{v}");
        assert!(v.contains("dict"), "does not imply the bare name is a string");

        // Facts and the value-less magic names stay silent — no "provided by Ansible" noise.
        assert!(hover("ansible_os_family").is_none());
        assert!(hover("inventory_hostname").is_none());
        assert!(hover("playbook_dir").is_none());

        // A known name whose value was not detected invents nothing.
        let empty = AnsibleInstall::default();
        assert!(super::injected_var_hover("ansible_playbook_python", &empty).is_none());
        assert!(super::injected_var_hover("ansible_version", &empty).is_none());
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
            const DEMOS: [&str; 2] = ["invalid_attributes.yml", "placement.yml"];
            if path.file_name().is_some_and(|n| DEMOS.iter().any(|d| n == *d)) {
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

    /// No false positives: every demo file except the one built to demonstrate the rule
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
        let (md, _) = super::Backend::variable_hover_at(&doc, &nodes, byte, &path)
            .expect("hover expected");
        assert!(md.contains("vars_files"), "provenance label in: {md}");
        assert!(md.contains("vars/shared.yml"), "defining file in: {md}");
        assert!(md.contains("https://api.internal:8443"), "value in: {md}");
    }

    /// T-066 against the real demo: `network_mtu` reaches the playbook only through
    /// provisioner's meta dependency on network-base, so its hover line carries the
    /// breadcrumb; `provisioner_user` comes from a role the playbook names directly, so
    /// its hover stays bare.
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
            super::Backend::variable_hover_at(&doc, &nodes, byte, &path)
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
            let (md, _) = super::Backend::variable_hover_at(&doc, &nodes, byte, &path)
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
        let (md, _) = super::Backend::variable_hover_at(&doc, &nodes, byte, &path)
            .expect("hover expected");
        println!("--- chain_included ---\n{md}\n");
        assert!(md.contains("include_vars"), "wrong source:\n{md}");
        assert!(md.contains("chain-c/vars/settings.yml"), "wrong file:\n{md}");
        assert!(plain(&md).contains("dependency of chain-b"), "missing inner hop:\n{md}");
        assert!(plain(&md).contains("dependency of chain-a"), "missing outer hop:\n{md}");
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
        let refs = ansible_core::references::extract(&nodes);
        let hover = |value: &str| {
            let r = refs.iter().find(|r| r.value == value).expect("ref in demo");
            let res = ansible_core::resolve::resolve_with(r, &ctx, &Default::default());
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

    /// T-029 box 4: a resolved module hovers one line of provenance — collection and
    /// origin — not the raw candidate paths. `ansible.builtin.debug` also has an action
    /// twin in core, so the documentation-only caveat must appear.
    #[test]
    fn hover_shows_module_provenance_not_paths() {
        if ansible_core::install::AnsibleInstall::detect().package_dir.is_none() {
            return; // ansible not on PATH
        }
        let path = std::path::Path::new("../../demo/tasks/modules.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = ansible_core::parse::Document::new(text);
        let nodes = doc.parse().unwrap();
        let ctx = ansible_core::workspace::FileContext::discover(&path);
        let refs = ansible_core::references::extract(&nodes);
        let r = refs
            .iter()
            .find(|r| r.value == "ansible.builtin.debug")
            .expect("ref in demo");
        let res = ansible_core::resolve::resolve_with(r, &ctx, &Default::default());
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
        let refs = ansible_core::references::extract(&nodes);
        let r = refs.iter().find(|r| r.value == "stage_files").expect("bare ref");
        let res = ansible_core::resolve::resolve_with(r, &ctx, &Default::default());
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
        let refs = ansible_core::references::extract(&nodes);
        let r = refs.iter().find(|r| r.value == "demo.charlie.beacon").expect("module ref");
        let res = ansible_core::resolve::resolve_with(r, &ctx, &Default::default());
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
        let refs = ansible_core::references::extract(&nodes);
        let r = refs.iter().find(|r| r.value == module).expect("module ref");
        let res = ansible_core::resolve::resolve_with(r, &ctx, &Default::default());
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
            let refs = ansible_core::references::extract(&nodes);
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
                super::hover_at(&doc, &nodes, &path, byte, settings)
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

        let md = super::hover_at(&doc, &nodes, &path, text.find("ping:").unwrap() + 1, settings)
            .expect("module hover expected")
            .0;
        assert!(md.contains("ansible.legacy"), "provenance, not the condition, in: {md}");

        // And the condition is still reachable — on its keyword.
        let kw = super::hover_at(&doc, &nodes, &path, text.find("when:").unwrap() + 1, settings)
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
        let refs = ansible_core::references::extract(&nodes);
        let r = refs.iter().find(|r| r.value == "purge_cache").expect("bare ref");
        let res = ansible_core::resolve::resolve_with(r, &ctx, &Default::default());
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
        let refs = ansible_core::references::extract(&nodes);
        let r = refs.iter().find(|r| r.value == "deploy_report").expect("bare ref");
        let res = ansible_core::resolve::resolve_with(r, &ctx, &Default::default());
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
        let refs = ansible_core::references::extract(&nodes);
        let r = refs.iter().find(|r| r.value == "ping").expect("bare ref");
        let res = ansible_core::resolve::resolve_with(r, &ctx, &Default::default());
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
        let refs = ansible_core::references::extract(&nodes);
        let r = refs.iter().find(|r| r.value == "docker_container").expect("bare ref");
        let res = ansible_core::resolve::resolve_with(r, &ctx, &Default::default());
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
}
