//! Thin LSP shim over `ansible-core`. All logic lives in the core crate.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use ansible_core::parse::{Document, Node};
use ansible_core::references::{self, Reference};
use ansible_core::resolve::{self, Resolution, Status};
use ansible_core::workspace::FileContext;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

struct Backend {
    client: Client,
    docs: Mutex<HashMap<Url, String>>,
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
        let path = uri.to_file_path().ok()?;
        let doc = Document::new(text);
        let nodes = doc.parse()?;
        let ctx = FileContext::discover(&path);
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
            self.client.publish_diagnostics(uri.clone(), vec![], None).await;
            return;
        };
        let diagnostics = a
            .refs
            .iter()
            .filter(|(_, res)| res.status == Status::Missing)
            .map(|(r, res)| {
                let (sl, sc) = a.doc.byte_to_lsp(r.span.start);
                let (el, ec) = a.doc.byte_to_lsp(r.span.end);
                let tried = res
                    .candidates
                    .iter()
                    .map(|c| format!("  {}", shorten(c, &a.ctx)))
                    .collect::<Vec<_>>()
                    .join("\n");
                Diagnostic {
                    range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
                    severity: Some(DiagnosticSeverity::WARNING),
                    source: Some("ansible-lsp".into()),
                    message: format!("no file found for `{}`. Tried:\n{tried}", r.value),
                    ..Default::default()
                }
            })
            .collect();
        self.client
            .publish_diagnostics(uri.clone(), diagnostics, None)
            .await;
    }

    fn reference_at(doc: &Document, nodes: &[Node], pos: Position) -> Option<Reference> {
        let byte = doc.lsp_to_byte(pos.line, pos.character);
        references::extract(nodes)
            .into_iter()
            .find(|r| r.span.start <= byte && byte <= r.span.end)
    }
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
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
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
        tokio::task::spawn_blocking(|| {
            ansible_core::install::AnsibleInstall::detect();
        });
        self.client
            .log_message(MessageType::INFO, "ansible-lsp ready")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
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
    let (service, socket) = LspService::new(|client| Backend {
        client,
        docs: Mutex::new(HashMap::new()),
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}
