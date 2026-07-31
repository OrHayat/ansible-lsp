//! Thin LSP shim over `ansible-core`. All logic lives in the core crate.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ansible_core::condition;
use ansible_core::mutation;
use ansible_core::parse::{Document, Node};
use ansible_core::references::{self, Reference, ReferenceKind};
use ansible_core::resolve::{self, rule_id, Resolution, Status};
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
}

/// User preferences that change what we volunteer, never what we report. Diagnostics are
/// deliberately not configurable here — a warning you asked for is not fluff, and turning
/// rules off belongs in a committed project file (T-025), not a per-machine setting.
#[derive(Clone, Copy)]
struct Settings {
    hints: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { hints: true }
    }
}

impl Settings {
    /// Reads `{ inlayHints: { enabled } }`. The client normalises both
    /// `initializationOptions` and `didChangeConfiguration` to this one shape, so the
    /// server doesn't have to know how VS Code nests things. Anything missing keeps its
    /// default rather than silently turning a feature off.
    fn from_json(v: &serde_json::Value) -> Self {
        let d = Self::default();
        let get = |key: &str, fallback: bool| {
            v.get("inlayHints")
                .and_then(|h| h.get(key))
                .and_then(|b| b.as_bool())
                .unwrap_or(fallback)
        };
        Self { hints: get("enabled", d.hints) }
    }
}

/// Server -> client: whether an Ansible install was found. Drives the client's status bar,
/// which (unlike a startup toast) stays visible until it's resolved.
enum AnsibleStatus {}
impl tower_lsp::lsp_types::notification::Notification for AnsibleStatus {
    type Params = serde_json::Value;
    const METHOD: &'static str = "ansible/status";
}

struct Backend {
    client: Client,
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
}

struct Analysis {
    doc: Document,
    ctx: FileContext,
    refs: Vec<(Reference, Resolution)>,
}

impl Backend {
    fn text_of(&self, uri: &Url) -> Option<String> {
        self.docs.lock().ok()?.get(uri).cloned()
    }

    /// Parse `uri` and resolve every reference in it. `None` when the file isn't open,
    /// isn't a real path, or doesn't parse.
    fn analyze(&self, uri: &Url) -> Option<Analysis> {
        let text = self.text_of(uri)?;
        Self::analyze_text(text, &uri.to_file_path().ok()?)
    }

    fn analyze_text(text: String, path: &Path) -> Option<Analysis> {
        let doc = Document::new(text);
        let nodes = doc.parse()?;
        let ctx = FileContext::discover(path);
        let refs = references::extract(&nodes)
            .into_iter()
            .map(|r| {
                let res = resolve::resolve(&r, &ctx);
                (r, res)
            })
            .collect();
        Some(Analysis { doc, ctx, refs })
    }

    /// Warn only on literal paths that resolved to nothing. Templated values and
    /// unsupported kinds stay silent — a warning you can't trust is worse than none.
    async fn publish_diagnostics(&self, uri: &Url) {
        let Some(a) = self.analyze(uri) else {
            // No analysis means the file didn't parse. Since the parser now matches Ansible's
            // (libyaml), a parse failure is a real one — a play that loads this file will
            // fail — so it's an error, not a silent gap.
            let diags = self.unparseable_diagnostic(uri);
            self.track(uri, &diags);
            self.client.publish_diagnostics(uri.clone(), diags, None).await;
            return;
        };
        let mut diagnostics = Self::diagnostics_of(&a);
        diagnostics.extend(self.mutated_condition_diagnostics(&a));
        self.track(uri, &diagnostics);
        self.client
            .publish_diagnostics(uri.clone(), diagnostics, None)
            .await;
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

    fn diagnostics_of(a: &Analysis) -> Vec<Diagnostic> {
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

        // Conditions that cannot work whatever the variables hold. Anchored on the
        // `when:` itself, and deduplicated: one task can hold several references
        // sharing one condition.
        let mut seen = HashSet::new();
        let broken: Vec<Diagnostic> = a
            .refs
            .iter()
            .filter_map(|(r, _)| Some((r, r.condition_span?)))
            .filter(|(_, span)| seen.insert(span.start))
            .flat_map(|(r, span)| {
                r.conditions
                    .iter()
                    .flat_map(|c| condition::problems(c, r.repeated))
                    .map(move |p| (p, span))
            })
            .filter(|(p, span)| !a.doc.is_suppressed(span.start, p.rule_id()))
            .map(|(p, span)| Diagnostic {
                range: range_of(span),
                severity: Some(DiagnosticSeverity::WARNING),
                source: Some("ansible-lsp".into()),
                code: Some(NumberOrString::String(p.rule_id().into())),
                message: p.message().into(),
                ..Default::default()
            })
            .collect();

        missing.chain(broken).collect()
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

    /// Resolve every YAML file in the workspace and publish what's broken.
    ///
    /// I/O bound (~3.5 s over 731 files) so it runs detached and publishes as it
    /// goes — the Problems panel fills progressively instead of appearing at the end.
    async fn scan_workspace(&self) {
        let roots = self.roots.lock().map(|r| r.clone()).unwrap_or_default();
        let stale: HashSet<Url> = self.flagged.lock().map(|f| f.clone()).unwrap_or_default();
        let mut still_flagged = HashSet::new();

        for root in roots {
            for path in yaml_files(&root) {
                // An open buffer is authoritative over what's on disk.
                let Ok(uri) = Url::from_file_path(&path) else { continue };
                if self.text_of(&uri).is_some() {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                let Some(a) = Self::analyze_text(text, &path) else { continue };
                let mut diagnostics = Self::diagnostics_of(&a);
                diagnostics.extend(self.mutated_condition_diagnostics(&a));
                if diagnostics.is_empty() {
                    continue;
                }
                still_flagged.insert(uri.clone());
                self.client.publish_diagnostics(uri, diagnostics, None).await;
            }
        }

        // Clear files that were flagged before but are clean now.
        for uri in stale.difference(&still_flagged) {
            self.client.publish_diagnostics(uri.clone(), vec![], None).await;
        }
        if let Ok(mut f) = self.flagged.lock() {
            *f = still_flagged;
        }
    }

    /// Every resolvable reference and how many files it reaches.
    async fn resolved_references(&self, p: ReferencesParams) -> Result<Vec<ResolvedRef>> {
        let Some(a) = self.analyze(&p.uri) else {
            return Ok(Vec::new());
        };
        Ok(a.refs
            .iter()
            .filter(|(_, res)| res.status == Status::Resolved)
            .map(|(r, res)| {
                let (sl, sc) = a.doc.byte_to_lsp(r.span.start);
                let (el, ec) = a.doc.byte_to_lsp(r.span.end);
                ResolvedRef {
                    range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
                    targets: res.targets.len(),
                }
            })
            .collect())
    }

    fn reference_at(doc: &Document, nodes: &[Node], pos: Position) -> Option<Reference> {
        let byte = doc.lsp_to_byte(pos.line, pos.character);
        references::extract(nodes)
            .into_iter()
            .find(|r| r.span.start <= byte && byte <= r.span.end)
    }
}

/// Hover markdown for a conditional reference: every clause spelled out in plain English
/// (or verbatim when it doesn't match a known shape), plus the reminder that an import's
/// `when:` fans out onto the whole imported file.
fn when_hover(r: &Reference) -> String {
    let raw = |c: &str| format!("`{}`", c.trim());
    let mut s = String::from("**`when:`**");
    if let [only] = r.conditions.as_slice() {
        // A single clause is the whole condition, so keep the "runs unless / only if"
        // framing that says what it decides.
        let line = condition::classify(only).label().unwrap_or_else(|| raw(only));
        s.push_str(&format!("\n\n{line}"));
    } else {
        // Listed clauses are ANDed. Say so, and state each as a bare requirement rather
        // than as its own "runs only if" sentence, which would read as standalone.
        s.push_str("\n\nRuns only when **all** hold:");
        for c in &r.conditions {
            let line = condition::classify(c).requirement().unwrap_or_else(|| raw(c));
            s.push_str(&format!("\n- {line}"));
        }
    }
    if r.kind == ReferenceKind::ImportPlaybook {
        s.push_str(
            "\n\n_The condition is copied onto every task in the imported playbook and \
             re-checked per task._",
        );
    }
    s
}

fn message_for(r: &Reference, res: &Resolution, ctx: &FileContext) -> String {
    // Listing a candidate path with `{{ }}` still in it explains nothing. The real
    // problem is that a static import is expanded before play variables exist.
    if r.templated && r.kind == ReferenceKind::ImportPlaybook {
        return format!(
            "`import_playbook` is expanded before play variables exist, so `{}` cannot \
             resolve. Only extra-vars (-e) are available here — play vars, host vars and \
             set_fact are not. Use one import per case with `when:`, or a dynamic \
             `include_tasks` inside a play.",
            r.value
        );
    }
    let tried = res
        .candidates
        .iter()
        .map(|c| format!("  {}", shorten(c, ctx)))
        .collect::<Vec<_>>()
        .join("\n");
    format!("no file found for `{}`. Tried:\n{tried}", r.value)
}

/// Candidate paths are absolute and long; show them relative to the project root.
fn shorten(p: &Path, ctx: &FileContext) -> String {
    ctx.project_root
        .as_ref()
        .and_then(|root| p.strip_prefix(root).ok())
        .unwrap_or(p)
        .display()
        .to_string()
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, p: InitializeParams) -> Result<InitializeResult> {
        if let Ok(mut roots) = self.roots.lock() {
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
                if let Ok(mut s) = self.settings.lock() {
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
        self.startup_note.lock().map(|mut n| *n = received).ok();

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
        // Detecting the ansible install shells out to `ansible --version` (~500 ms).
        // Warm it here, off the request path, so the first documentLink isn't slow.
        let install = tokio::task::spawn_blocking(|| {
            ansible_core::install::AnsibleInstall::detect().clone()
        })
        .await
        .unwrap_or_default();
        // No install means builtins and installed collections can't resolve — say so, so a
        // plain `ansible.builtin.debug` that won't jump reads as "no Ansible here", not "the
        // tool is broken". In-repo files, roles, and modules still work. The status
        // notification drives a persistent status-bar item; the toast is the immediate nudge.
        let found = install.package_dir.is_some();
        let _ = self
            .client
            .send_notification::<AnsibleStatus>(serde_json::json!({ "found": found }))
            .await;
        if !found {
            self.client
                .show_message(
                    MessageType::WARNING,
                    "Ansible not found on PATH — builtin modules (ansible.builtin.*) and \
                     installed collections won't resolve. In-repo files, roles, and modules \
                     still work. Install ansible-core (WSL on Windows).",
                )
                .await;
        }
        let note = self.startup_note.lock().map(|n| n.clone()).unwrap_or_default();
        let s = self.settings.lock().map(|s| *s).unwrap_or_default();
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
        self.scan_workspace().await;
    }

    async fn did_change_configuration(&self, p: DidChangeConfigurationParams) {
        if let Ok(mut s) = self.settings.lock() {
            *s = Settings::from_json(&p.settings);
        }
        let s = self.settings.lock().map(|s| *s).unwrap_or_default();
        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "settings changed — received: {} | effective: enabled={}",
                    p.settings, s.hints
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
        let settings = self.settings.lock().map(|s| *s).unwrap_or_default();
        if !settings.hints {
            return Ok(None);
        }
        let uri = &p.text_document_position_params.text_document.uri;
        let pos = p.text_document_position_params.position;
        let Some(a) = self.analyze(uri) else {
            return Ok(None);
        };
        let byte = a.doc.lsp_to_byte(pos.line, pos.character);
        // Anchor on the reference value (the import path), not on `when:`: it's the thing
        // you point at, and it's already painted teal as clickable.
        let Some((r, _)) = a.refs.iter().find(|(r, _)| {
            !r.conditions.is_empty() && r.span.start <= byte && byte <= r.span.end
        }) else {
            return Ok(None);
        };
        let (sl, sc) = a.doc.byte_to_lsp(r.span.start);
        let (el, ec) = a.doc.byte_to_lsp(r.span.end);
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: when_hover(r),
            }),
            range: Some(Range::new(Position::new(sl, sc), Position::new(el, ec))),
        }))
    }

    async fn did_open(&self, p: DidOpenTextDocumentParams) {
        let uri = p.text_document.uri;
        if let Ok(mut d) = self.docs.lock() {
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
        if let Ok(mut d) = self.docs.lock() {
            d.insert(uri.clone(), change.text);
        }
        self.publish_diagnostics(&uri).await;
    }

    async fn did_close(&self, p: DidCloseTextDocumentParams) {
        if let Ok(mut d) = self.docs.lock() {
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

        let Some(text) = self.text_of(&uri) else {
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
        let Some(a) = self.analyze(&p.text_document.uri) else {
            return Ok(None);
        };
        let links = a
            .refs
            .iter()
            .filter(|(_, res)| res.status == Status::Resolved)
            // Exactly one target only. A link's target wins over the definition
            // provider on Cmd+click, so emitting one for a multi-candidate templated
            // path would silently drop the other candidates.
            .filter(|(_, res)| res.targets.len() == 1)
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
        docs: Mutex::new(HashMap::new()),
        roots: Mutex::new(Vec::new()),
        flagged: Mutex::new(HashSet::new()),
        mutations: Mutex::new(HashMap::new()),
        settings: Mutex::new(Settings::default()),
        startup_note: Mutex::new(String::new()),
    })
    .custom_method("ansible/references", Backend::resolved_references)
    .finish();
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use super::Settings;

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
